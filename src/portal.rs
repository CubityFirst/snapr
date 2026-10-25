//! The desktop's portals (xdg-desktop-portal), on Linux: making requests,
//! and the permission to take screenshots on Wayland.
//!
//! On Wayland, xcap captures the screen through the screenshot portal. (It
//! tries GNOME Shell's own interface first, which only GNOME's tools may
//! use, and wlroots' protocol last, which GNOME and KDE don't have, so its
//! error then is a misleading "Cannot find required wayland protocol".) The
//! first time an app takes a screenshot without the desktop's screenshot
//! UI, the portal asks the user, and GNOME only shows that dialog for the
//! focused app: a capture from the hotkey or the tray, with no snapr window
//! focused, is refused without anyone being asked. So snapr asks from its
//! window, when the user clicks to.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use zbus::blocking::{Connection, Proxy, proxy::Builder};
use zbus::proxy::CacheProperties;
use zbus::zvariant::{OwnedValue, Value};

pub const DESTINATION: &str = "org.freedesktop.portal.Desktop";
pub const PATH: &str = "/org/freedesktop/portal/desktop";

const STORE: &str = "org.freedesktop.impl.portal.PermissionStore";
const STORE_PATH: &str = "/org/freedesktop/impl/portal/PermissionStore";

/// What to do when the desktop hasn't let snapr take screenshots.
pub const SCREENSHOTS_OFF: &str = "the desktop hasn't let snapr take screenshots: click Allow \
    screenshots in snapr's window, then Allow when the desktop asks";

/// What the screenshot portal has stored for snapr.
#[derive(Debug, PartialEq, Eq)]
enum Permission {
    /// Not asked yet: the portal asks on the next screenshot.
    Unset,
    Yes,
    /// Refused: the portal refuses without asking again.
    No,
}

/// Whether the desktop will refuse snapr's screenshots: a Wayland session,
/// and the portal's permission not given. `false` if it can't tell.
pub fn screenshots_blocked() -> bool {
    wayland() && permission().is_some_and(|p| p != Permission::Yes)
}

/// Whether this is a GNOME session, where captures from the hotkey or the
/// tray can't get the permission (see the module's docs).
pub fn gnome() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.split(':').any(|d| d == "GNOME"))
}

/// As xcap decides whether to capture through the portal.
fn wayland() -> bool {
    std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland")
        || std::env::var("WAYLAND_DISPLAY").is_ok_and(|d| d.to_lowercase().contains("wayland"))
}

/// The screenshot permission the portal has for snapr, if the permission
/// store can be read.
fn permission() -> Option<Permission> {
    let conn = Connection::session().ok()?;
    let reply = conn.call_method(
        Some(STORE),
        STORE_PATH,
        Some(STORE),
        "Lookup",
        &("screenshot", "screenshot"),
    );
    let table = match reply {
        Ok(reply) => reply
            .body()
            .deserialize::<(HashMap<String, Vec<String>>, OwnedValue)>()
            .ok()?
            .0,
        // No app has been asked yet.
        Err(zbus::Error::MethodError(name, ..))
            if name.as_str() == "org.freedesktop.portal.Error.NotFound" =>
        {
            HashMap::new()
        }
        Err(_) => return None,
    };
    Some(match table.get(&app_id()).and_then(|p| p.first()).map(String::as_str) {
        Some("yes") => Permission::Yes,
        Some("no") => Permission::No,
        _ => Permission::Unset,
    })
}

/// Has the desktop ask the user to let snapr take screenshots, by taking
/// one through the portal without its UI as captures do, and deletes the
/// picture. Call it while snapr's window is focused (from a click in it);
/// it waits for the user's answer. An earlier "no" is cleared first, since
/// the portal never asks again after one and the user has just asked to be.
pub fn ask_for_screenshots() -> Result<(), String> {
    // Not the hotkeys' connection, which tells the portal snapr's id: the
    // portal has to see snapr as it sees xcap's captures.
    let conn = Connection::session().map_err(|e| e.to_string())?;
    if permission() == Some(Permission::No) {
        let _ = conn.call_method(
            Some(STORE),
            STORE_PATH,
            Some(STORE),
            "DeletePermission",
            &("screenshot", "screenshot", app_id()),
        );
    }
    let screenshot =
        proxy(&conn, PATH, "org.freedesktop.portal.Screenshot").map_err(|e| e.to_string())?;
    let handle = token();
    let options = HashMap::from([
        ("handle_token", Value::from(handle.as_str())),
        ("interactive", Value::from(false)),
    ]);
    let results = request(&conn, &handle, || {
        screenshot.call_method("Screenshot", &("", options))
    })?;
    if let Some(Value::Str(uri)) = results.get("uri").map(|v| &**v)
        && let Some(path) = file_path(uri)
    {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

/// The id the portal knows snapr by: worked out, as the portal does, from
/// the systemd unit the desktop started it in, e.g. `snapr` for
/// `app-gnome-snapr-1234.scope` or `app-snapr@1234.service`. Empty when
/// started some other way, e.g. from a terminal.
fn app_id() -> String {
    std::fs::read_to_string("/proc/self/cgroup")
        .ok()
        .and_then(|c| unit_app_id(&c))
        .unwrap_or_default()
}

/// The app id in a `/proc/<pid>/cgroup`'s unit name:
/// `app[-<launcher>]-<id>-<random>.scope` or
/// `app[-<launcher>]-<id>[@<random>].service`, with `-` in the id
/// escaped as `\x2d`.
fn unit_app_id(cgroup: &str) -> Option<String> {
    let unit = cgroup.lines().find_map(|l| l.strip_prefix("0::"))?.rsplit('/').next()?;
    let name = unit.strip_prefix("app-")?;
    let name = match name.strip_suffix(".scope") {
        Some(scope) => scope.rsplit_once('-')?.0,
        None => {
            let service = name.strip_suffix(".service")?;
            service.split_once('@').map_or(service, |(name, _)| name)
        }
    };
    let id = name.rsplit('-').next().filter(|id| !id.is_empty())?;
    Some(id.replace("\\x2d", "-"))
}

/// The file a `file://` URI names.
fn file_path(uri: &str) -> Option<PathBuf> {
    let path = uri.strip_prefix("file://")?.as_bytes();
    let mut bytes = Vec::with_capacity(path.len());
    let mut i = 0;
    while i < path.len() {
        if path[i] == b'%'
            && let Some(byte) = path
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            bytes.push(byte);
            i += 3;
        } else {
            bytes.push(path[i]);
            i += 1;
        }
    }
    use std::os::unix::ffi::OsStringExt;
    Some(std::ffi::OsString::from_vec(bytes).into())
}

/// Makes a portal call that answers with a Request's Response signal, and
/// waits for the answer, which may take a while if the desktop asks the user.
pub fn request(
    conn: &Connection,
    token: &str,
    call: impl FnOnce() -> zbus::Result<zbus::Message>,
) -> Result<HashMap<String, OwnedValue>, String> {
    // The request's path is known in advance, so the answer can't be missed.
    let sender = conn
        .unique_name()
        .ok_or("not connected to D-Bus")?
        .trim_start_matches(':')
        .replace('.', "_");
    let path = format!("{PATH}/request/{sender}/{token}");
    let request = proxy(conn, path, "org.freedesktop.portal.Request").map_err(|e| e.to_string())?;
    let mut responses = request
        .receive_signal("Response")
        .map_err(|e| e.to_string())?;
    call().map_err(|e| e.to_string())?;
    let response = responses.next().ok_or("the portal stopped")?;
    let (code, results) = response
        .body()
        .deserialize::<(u32, HashMap<String, OwnedValue>)>()
        .map_err(|e| e.to_string())?;
    match code {
        0 => Ok(results),
        1 => Err("the desktop's dialog was cancelled".into()),
        _ => Err("the desktop refused".into()),
    }
}

pub fn proxy<'a>(
    conn: &Connection,
    path: impl TryInto<zbus::zvariant::ObjectPath<'a>, Error: Into<zbus::Error>>,
    interface: &'a str,
) -> zbus::Result<Proxy<'a>> {
    Builder::new(conn)
        .destination(DESTINATION)?
        .path(path)?
        .interface(interface)?
        .cache_properties(CacheProperties::No)
        .build()
}

const SETTINGS: &str = "org.freedesktop.portal.Settings";
const APPEARANCE: &str = "org.freedesktop.appearance";

/// Passes `changed` whether the desktop's style is dark (its appearance
/// `color-scheme`: 1 is dark; 2, light, and 0, no preference, aren't), now
/// and whenever it changes, from a thread of its own. Nothing happens if the
/// desktop has no settings portal.
pub fn watch_dark_style(changed: impl Fn(bool) + Send + 'static) {
    std::thread::spawn(move || {
        let watch = || -> zbus::Result<()> {
            let conn = Connection::session()?;
            let settings = proxy(&conn, PATH, SETTINGS)?;
            // Listening before reading, so a change in between isn't missed.
            let signals = settings.receive_signal("SettingChanged")?;
            // ReadOne (version 2) answers with the value; Read, which older
            // portals have instead, wraps it in another variant.
            let value: zbus::Result<OwnedValue> = settings
                .call("ReadOne", &(APPEARANCE, "color-scheme"))
                .or_else(|_| settings.call("Read", &(APPEARANCE, "color-scheme")));
            if let Some(dark) = value.ok().and_then(|v| dark_style(&v)) {
                changed(dark);
            }
            for signal in signals {
                let (namespace, key, value): (String, String, OwnedValue) =
                    signal.body().deserialize()?;
                if namespace == APPEARANCE
                    && key == "color-scheme"
                    && let Some(dark) = dark_style(&value)
                {
                    changed(dark);
                }
            }
            Ok(())
        };
        if let Err(e) = watch() {
            eprintln!("couldn't follow the desktop's dark style: {e}");
        }
    });
}

fn dark_style(value: &Value) -> Option<bool> {
    match value {
        Value::U32(scheme) => Some(*scheme == 1),
        Value::Value(inner) => dark_style(inner),
        _ => None,
    }
}

/// A fresh handle token, the last element of a request or session path.
pub fn token() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!("snapr{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id_of(unit: &str) -> Option<String> {
        unit_app_id(&format!(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/{unit}\n"
        ))
    }

    #[test]
    fn reads_app_ids_from_units() {
        assert_eq!(id_of("app-gnome-snapr-7285.scope").as_deref(), Some("snapr"));
        assert_eq!(id_of("app-snapr-7285.scope").as_deref(), Some("snapr"));
        assert_eq!(id_of("app-snapr@a1b2.service").as_deref(), Some("snapr"));
        assert_eq!(id_of("app-gnome-snapr.service").as_deref(), Some("snapr"));
        assert_eq!(
            id_of("app-flatpak-org.example.my\\x2dapp-99.scope").as_deref(),
            Some("org.example.my-app")
        );
        assert_eq!(id_of("vte-spawn-1234.scope"), None);
        assert_eq!(id_of("app-7285.scope"), None);
    }

    #[test]
    fn reads_the_dark_style() {
        assert_eq!(dark_style(&Value::U32(1)), Some(true));
        assert_eq!(dark_style(&Value::U32(0)), Some(false));
        assert_eq!(dark_style(&Value::U32(2)), Some(false));
        // As Read gives it.
        assert_eq!(dark_style(&Value::Value(Box::new(Value::U32(1)))), Some(true));
        assert_eq!(dark_style(&Value::from("dark")), None);
    }

    #[test]
    fn decodes_file_uris() {
        assert_eq!(
            file_path("file:///home/a/Pictures/Screenshot%20From%202026.png"),
            Some(PathBuf::from("/home/a/Pictures/Screenshot From 2026.png"))
        );
        assert_eq!(file_path("https://example.com/a.png"), None);
    }
}
