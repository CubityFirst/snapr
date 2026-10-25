//! Global hotkeys through the desktop's GlobalShortcuts portal, for Wayland,
//! where an app can't watch the keyboard itself (an X11 grab only sees keys
//! typed into other X11 apps). The compositor handles the keys, so they also
//! work for remote desktop input, which never reaches `/dev/input`.
//!
//! The desktop owns the keys: snapr's are only suggested the first time it
//! sees each shortcut (GNOME asks the user to confirm them), and after that
//! they're changed in the desktop's settings.
//!
//! All the hotkeys share one portal session. GNOME grabs every shortcut it
//! has stored for snapr in each session, and a session's shortcuts can only
//! be bound once, so any change closes it and binds them all again.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use livesplit_hotkey::{Hotkey, Modifiers};
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::portal::{DESTINATION, PATH, proxy, request, token};

const INTERFACE: &str = "org.freedesktop.portal.GlobalShortcuts";
/// The portal names apps by their desktop file, `snapr.desktop`.
const APP_ID: &str = "snapr";
/// How long to wait for more changes before binding: the hotkeys are set one
/// after another at startup and when settings are saved, and each bind can
/// show a dialog.
const SETTLE: Duration = Duration::from_millis(150);

type Callback = Arc<dyn Fn() + Send + Sync>;

struct Slot {
    /// Names the shortcut to the desktop, which remembers its keys by it.
    id: String,
    description: String,
    /// Whether it's offered to the desktop.
    wanted: bool,
    /// The keys suggested for it, if any.
    hotkey: Option<Hotkey>,
    callback: Callback,
}

#[derive(Default)]
struct State {
    slots: HashMap<u64, Slot>,
    next_slot: u64,
    /// The session whose shortcuts are bound, if any.
    session: Option<OwnedObjectPath>,
    /// How the desktop describes each bound shortcut's keys, by id.
    keys: HashMap<String, String>,
    /// A bind is waiting for the desktop, which may be asking the user.
    binding: bool,
}

/// A shortcut as it's bound: id, description and suggested keys.
type Wanted = (String, String, Option<String>);

pub struct Portal {
    conn: Connection,
    version: u32,
    state: Arc<Mutex<State>>,
    changed: Sender<()>,
}

impl Portal {
    /// The portal, if the desktop has one with global shortcuts.
    pub fn get() -> Option<&'static Portal> {
        static PORTAL: OnceLock<Option<Portal>> = OnceLock::new();
        PORTAL
            .get_or_init(|| {
                Self::start()
                    .map_err(|e| eprintln!("global shortcuts portal unavailable: {e}"))
                    .ok()
            })
            .as_ref()
    }

    fn start() -> zbus::Result<Self> {
        let conn = Connection::session()?;
        // An app the portal can't place by its systemd unit (e.g. started
        // from a terminal) has to say which it is, before any portal call.
        // Older portals don't have this, and it fails if the portal already
        // knows; either way the portal goes on as before.
        let _ = conn.call_method(
            Some(DESTINATION),
            PATH,
            Some("org.freedesktop.host.portal.Registry"),
            "Register",
            &(APP_ID, HashMap::<&str, Value>::new()),
        );
        let shortcuts = proxy(&conn, PATH, INTERFACE)?;
        let version = shortcuts.get_property::<u32>("version")?;
        let activated = shortcuts.receive_signal("Activated")?;
        let keys_changed = shortcuts.receive_signal("ShortcutsChanged")?;

        let state: Arc<Mutex<State>> = Arc::default();
        let (changed, changes) = mpsc::channel();
        let pressed = state.clone();
        thread::Builder::new()
            .name("shortcuts".into())
            .spawn(move || {
                let _shortcuts = shortcuts;
                for msg in activated {
                    let Ok((session, id, _, _)) = msg.body().deserialize::<(
                        OwnedObjectPath,
                        String,
                        u64,
                        HashMap<String, OwnedValue>,
                    )>() else {
                        continue;
                    };
                    let callback = {
                        let state = pressed.lock().unwrap();
                        if state.session.as_ref() != Some(&session) {
                            continue;
                        }
                        state
                            .slots
                            .values()
                            .find(|s| s.id == id && s.wanted)
                            .map(|s| s.callback.clone())
                    };
                    if let Some(callback) = callback {
                        callback();
                    }
                }
            })
            .map_err(|e| zbus::Error::Failure(e.to_string()))?;
        // The user changed the keys in the desktop's settings.
        let changed_keys = state.clone();
        thread::Builder::new()
            .name("shortcuts-changed".into())
            .spawn(move || {
                for msg in keys_changed {
                    let Ok((session, list)) = msg.body().deserialize::<(
                        OwnedObjectPath,
                        Vec<(String, HashMap<String, OwnedValue>)>,
                    )>() else {
                        continue;
                    };
                    {
                        let mut state = changed_keys.lock().unwrap();
                        if state.session.as_ref() != Some(&session) {
                            continue;
                        }
                        state.keys = keys(list.iter().map(|(id, details)| (id, details)));
                    }
                    super::keys_changed();
                }
            })
            .map_err(|e| zbus::Error::Failure(e.to_string()))?;
        let bound = state.clone();
        let bind_conn = conn.clone();
        thread::Builder::new()
            .name("shortcuts-bind".into())
            .spawn(move || run(bind_conn, bound, changes))
            .map_err(|e| zbus::Error::Failure(e.to_string()))?;
        Ok(Self {
            conn,
            version,
            state,
            changed,
        })
    }

    /// How the desktop describes the keys bound to the shortcut `id`, e.g.
    /// "Press <Control>j". `None` if it has none, or isn't bound yet.
    pub fn keys(&self, id: &str) -> Option<String> {
        self.state.lock().unwrap().keys.get(id).cloned()
    }

    /// Whether a bind is waiting for the desktop: GNOME asks the user about
    /// new shortcuts first. Meanwhile none of them work.
    pub fn binding(&self) -> bool {
        self.state.lock().unwrap().binding
    }

    /// Asks the desktop to show its settings for snapr's shortcuts. Not all
    /// desktops can (GNOME 50 can't).
    pub fn configure(&self) -> Result<(), String> {
        if self.version < 2 {
            return Err("the desktop can't show its shortcut settings".into());
        }
        let session = self
            .state
            .lock()
            .unwrap()
            .session
            .clone()
            .ok_or("no hotkeys are bound")?;
        let shortcuts = proxy(&self.conn, PATH, INTERFACE).map_err(|e| e.to_string())?;
        shortcuts
            .call_method(
                "ConfigureShortcuts",
                &(&session, "", HashMap::<&str, Value>::new()),
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Adds a shortcut, unbound until it's given a hotkey.
    pub fn add(&'static self, id: &str, description: &str, callback: super::Callback) -> Shortcut {
        let callback = Mutex::new(callback);
        let mut state = self.state.lock().unwrap();
        let slot = state.next_slot;
        state.next_slot += 1;
        state.slots.insert(
            slot,
            Slot {
                id: id.into(),
                description: description.into(),
                wanted: false,
                hotkey: None,
                callback: Arc::new(move || (callback.lock().unwrap())()),
            },
        );
        Shortcut { portal: self, slot }
    }

    fn update(&self, change: impl FnOnce(&mut State)) {
        change(&mut self.state.lock().unwrap());
        let _ = self.changed.send(());
    }
}

/// One of snapr's hotkeys, bound through the portal.
pub struct Shortcut {
    portal: &'static Portal,
    slot: u64,
}

impl Shortcut {
    /// Asks for `hotkey`. Binding happens shortly after, on another thread,
    /// and problems are reported through `super::report`.
    pub fn register(&self, hotkey: Hotkey) -> Result<(), String> {
        self.set(true, Some(hotkey));
        Ok(())
    }

    /// Offers the shortcut with no keys suggested: the desktop asks for some.
    pub fn offer(&self) {
        self.set(true, None);
    }

    pub fn unregister(&self) {
        self.set(false, None);
    }

    fn set(&self, wanted: bool, hotkey: Option<Hotkey>) {
        let slot = self.slot;
        self.portal.update(|s| {
            if let Some(s) = s.slots.get_mut(&slot) {
                s.wanted = wanted;
                s.hotkey = hotkey;
            }
        });
    }
}

impl Drop for Shortcut {
    fn drop(&mut self) {
        let slot = self.slot;
        self.portal.update(|s| {
            s.slots.remove(&slot);
        });
    }
}

/// Binds the shortcuts again each time they change.
fn run(conn: Connection, state: Arc<Mutex<State>>, changes: Receiver<()>) {
    let mut bound: Vec<Wanted> = Vec::new();
    while changes.recv().is_ok() {
        while changes.recv_timeout(SETTLE).is_ok() {}
        let wanted = wanted(&state.lock().unwrap());
        if wanted == bound {
            continue;
        }
        let old = state.lock().unwrap().session.take();
        if let Some(old) = old {
            // Frees its keys.
            let _ = conn.call_method(
                Some(DESTINATION),
                old.as_str(),
                Some("org.freedesktop.portal.Session"),
                "Close",
                &(),
            );
        }
        bound.clear();
        {
            let mut s = state.lock().unwrap();
            s.keys.clear();
            s.binding = !wanted.is_empty();
        }
        super::keys_changed();
        if wanted.is_empty() {
            continue;
        }
        let result = bind(&conn, &wanted);
        state.lock().unwrap().binding = false;
        super::keys_changed();
        match result {
            Ok((session, keys, missing)) => {
                let mut s = state.lock().unwrap();
                s.session = Some(session);
                s.keys = keys;
                drop(s);
                super::keys_changed();
                bound = wanted;
                // Left without keys, which the Hotkeys tab shows.
                if !missing.is_empty() {
                    eprintln!("hotkeys without keys: {}", missing.join(", "));
                }
            }
            Err(e) => super::report(format!("Couldn't set up global hotkeys: {e}")),
        }
    }
}

/// The shortcuts offered to the desktop, in a stable order.
fn wanted(state: &State) -> Vec<Wanted> {
    let mut wanted: Vec<Wanted> = state
        .slots
        .values()
        .filter(|s| s.wanted)
        .map(|s| {
            (
                s.id.clone(),
                s.description.clone(),
                s.hotkey.and_then(trigger),
            )
        })
        .collect();
    wanted.sort();
    wanted.dedup_by(|a, b| a.0 == b.0);
    wanted
}

/// A bound session, how the desktop describes the keys (see `keys`), and the
/// descriptions of the shortcuts it left out.
type Bound = (OwnedObjectPath, HashMap<String, String>, Vec<String>);

/// Opens a session and binds `wanted` to it.
fn bind(conn: &Connection, wanted: &[Wanted]) -> Result<Bound, String> {
    let shortcuts = proxy(conn, PATH, INTERFACE).map_err(|e| e.to_string())?;

    let handle = token();
    let session_token = token();
    let mut options = HashMap::from([("handle_token", Value::from(handle.as_str()))]);
    options.insert("session_handle_token", Value::from(session_token.as_str()));
    let results = request(conn, &handle, || {
        shortcuts.call_method("CreateSession", &(options,))
    })?;
    // A string, though the spec meant an object path.
    let session = match results.get("session_handle").map(|v| &**v) {
        Some(Value::Str(s)) => OwnedObjectPath::try_from(s.as_str()).ok(),
        Some(Value::ObjectPath(p)) => Some(p.clone().into()),
        _ => None,
    }
    .ok_or("the portal didn't open a session")?;

    let list: Vec<(&str, HashMap<&str, Value>)> = wanted
        .iter()
        .map(|(id, description, trigger)| {
            let mut details = HashMap::from([("description", Value::from(description.as_str()))]);
            if let Some(t) = trigger {
                details.insert("preferred_trigger", Value::from(t.as_str()));
            }
            (id.as_str(), details)
        })
        .collect();
    let handle = token();
    let options = HashMap::from([("handle_token", Value::from(handle.as_str()))]);
    let results = request(conn, &handle, || {
        shortcuts.call_method("BindShortcuts", &(&session, list, "", options))
    })?;

    let mut bound = Vec::new();
    if let Some(Value::Array(list)) = results.get("shortcuts").map(|v| &**v) {
        for item in list.iter() {
            if let Value::Structure(s) = item
                && let [Value::Str(id), Value::Dict(details)] = s.fields()
                && let Ok(details) = HashMap::<String, OwnedValue>::try_from(
                    details.try_clone().map_err(|e| e.to_string())?,
                )
            {
                bound.push((id.to_string(), details));
            }
        }
    }
    let missing = wanted
        .iter()
        .filter(|(id, ..)| !bound.iter().any(|(b, _)| b == id))
        .map(|(_, description, _)| description.clone())
        .collect();
    let keys = keys(bound.iter().map(|(id, details)| (id, details)));
    Ok((session, keys, missing))
}

/// The `trigger_description` of each of the desktop's shortcuts that has
/// one, by id.
fn keys<'a>(
    list: impl Iterator<Item = (&'a String, &'a HashMap<String, OwnedValue>)>,
) -> HashMap<String, String> {
    list.filter_map(|(id, details)| {
        let keys = details.get("trigger_description")?;
        let keys = <&str>::try_from(&**keys).ok()?;
        Some((id.clone(), keys.to_string()))
    })
    .collect()
}

/// The hotkey as the XDG shortcuts spec writes it, e.g. `CTRL+SHIFT+s`.
/// `None` for keys it has no name for: the desktop then asks for one.
fn trigger(hotkey: Hotkey) -> Option<String> {
    let mut trigger = String::new();
    for (m, name) in [
        (Modifiers::CONTROL, "CTRL"),
        (Modifiers::ALT, "ALT"),
        (Modifiers::SHIFT, "SHIFT"),
        (Modifiers::META, "LOGO"),
    ] {
        if hotkey.modifiers.contains(m) {
            trigger.push_str(name);
            trigger.push('+');
        }
    }
    trigger.push_str(&keysym(hotkey.key_code.name())?);
    Some(trigger)
}

/// The XKB keysym name for a key name such as `KeyS` or `F5`.
fn keysym(name: &str) -> Option<String> {
    if let Some(c) = name.strip_prefix("Key").filter(|c| c.len() == 1) {
        return Some(c.to_ascii_lowercase());
    }
    if let Some(d) = name.strip_prefix("Digit").filter(|d| d.len() == 1) {
        return Some(d.into());
    }
    if let Some(d) = name.strip_prefix("Numpad").filter(|d| d.len() == 1) {
        return Some(format!("KP_{d}"));
    }
    if let Some(n) = name.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()) {
        return (1..=24).contains(&n).then(|| format!("F{n}"));
    }
    let keysym = match name {
        "PrintScreen" => "Print",
        "Pause" => "Pause",
        "ScrollLock" => "Scroll_Lock",
        "Space" => "space",
        "Enter" => "Return",
        "Tab" => "Tab",
        "Escape" => "Escape",
        "Backspace" => "BackSpace",
        "Insert" => "Insert",
        "Delete" => "Delete",
        "Home" => "Home",
        "End" => "End",
        "PageUp" => "Page_Up",
        "PageDown" => "Page_Down",
        "ArrowUp" => "Up",
        "ArrowDown" => "Down",
        "ArrowLeft" => "Left",
        "ArrowRight" => "Right",
        "Minus" => "minus",
        "Equal" => "equal",
        "BracketLeft" => "bracketleft",
        "BracketRight" => "bracketright",
        "Backslash" => "backslash",
        "Semicolon" => "semicolon",
        "Quote" => "apostrophe",
        "Backquote" => "grave",
        "Comma" => "comma",
        "Period" => "period",
        "Slash" => "slash",
        "NumpadAdd" => "KP_Add",
        "NumpadSubtract" => "KP_Subtract",
        "NumpadMultiply" => "KP_Multiply",
        "NumpadDivide" => "KP_Divide",
        "NumpadDecimal" => "KP_Decimal",
        _ => return None,
    };
    Some(keysym.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trigger_of(spec: &str) -> Option<String> {
        trigger(spec.parse().unwrap())
    }

    #[test]
    fn writes_xdg_triggers() {
        assert_eq!(trigger_of("Ctrl + KeyJ").as_deref(), Some("CTRL+j"));
        assert_eq!(
            trigger_of("Alt + Shift + KeyS").as_deref(),
            Some("ALT+SHIFT+s")
        );
        assert_eq!(
            trigger_of("Ctrl + Meta + Digit4").as_deref(),
            Some("CTRL+LOGO+4")
        );
        assert_eq!(trigger_of("PrintScreen").as_deref(), Some("Print"));
        assert_eq!(trigger_of("Shift + ArrowUp").as_deref(), Some("SHIFT+Up"));
        assert_eq!(trigger_of("Ctrl + Numpad7").as_deref(), Some("CTRL+KP_7"));
        assert_eq!(trigger_of("Ctrl + F12").as_deref(), Some("CTRL+F12"));
    }

    #[test]
    fn leaves_unknown_keys_to_the_desktop() {
        assert_eq!(trigger_of("Ctrl + MediaPlayPause"), None);
    }

    #[test]
    fn binds_each_id_once() {
        let callback: Callback = Arc::new(|| {});
        let mut state = State::default();
        for (i, (id, hotkey)) in [
            ("b", Some("Ctrl + KeyB")),
            ("a", Some("Ctrl + KeyA")),
            ("a", Some("Ctrl + KeyC")),
            ("c", None),
        ]
        .into_iter()
        .enumerate()
        {
            state.slots.insert(
                i as u64,
                Slot {
                    id: id.into(),
                    description: id.into(),
                    wanted: hotkey.is_some(),
                    hotkey: hotkey.map(|h| h.parse().unwrap()),
                    callback: callback.clone(),
                },
            );
        }
        let ids: Vec<_> = wanted(&state).into_iter().map(|w| w.0).collect();
        assert_eq!(ids, ["a", "b"]);
    }
}
