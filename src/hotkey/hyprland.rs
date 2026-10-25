//! Keys for the portal's shortcuts on Hyprland. Its portal registers them
//! (as `snapr:<id>`) but ignores the keys apps suggest: they come from
//! `bind = …, global, snapr:<id>` lines in the user's config. So snapr adds
//! those binds to the running Hyprland itself, from the keys in its own
//! settings, through Hyprland's IPC socket. They aren't written to any file:
//! a config reload drops them, so they're added again then, and snapr
//! removes them when it quits.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use serde::Deserialize;

/// The portal's name for snapr, which Hyprland puts before each shortcut id.
const APP_ID: &str = "snapr";
/// How long to wait for Hyprland to answer a request.
const TIMEOUT: Duration = Duration::from_secs(2);

/// A key combination bound to one of snapr's shortcuts.
#[derive(Debug, Clone, PartialEq)]
struct Bind {
    id: String,
    /// As Hyprland writes them, e.g. `CTRL SHIFT`.
    mods: String,
    /// Hyprland's bit for each of `mods`, as `hyprctl binds` gives them.
    modmask: u32,
    /// An XKB keysym name, e.g. `s` or `Print`.
    key: String,
}

#[derive(Default)]
struct State {
    /// The binds snapr's settings ask for.
    wanted: Vec<Bind>,
    /// The binds snapr added, which it takes away again.
    added: Vec<Bind>,
    /// The wanted binds whose keys Hyprland has bound to something else,
    /// said once each.
    taken: Vec<Bind>,
}

/// Whether snapr runs under Hyprland.
pub fn running() -> bool {
    dir().is_some()
}

/// Binds the keys of each shortcut in `wanted` (its id and XDG trigger, see
/// `portal::trigger`) and unbinds those snapr added that aren't any more.
/// Problems are reported through `super::report`.
pub fn apply(wanted: &[(String, Option<String>)]) {
    let Some(state) = state() else { return };
    let mut state = state.lock().unwrap();
    state.wanted = wanted
        .iter()
        .filter_map(|(id, trigger)| bind(id, trigger.as_deref()?))
        .collect();
    sync(&mut state);
}

/// Takes away the binds snapr added, as it quits: they'd go on catching
/// their keys, for nothing.
pub fn clear() {
    let Some(state) = STATE.get().and_then(Option::as_ref) else { return };
    let mut state = state.lock().unwrap();
    state.wanted.clear();
    sync(&mut state);
}

static STATE: OnceLock<Option<Mutex<State>>> = OnceLock::new();

/// The binds' state, watching for config reloads from the first call.
fn state() -> Option<&'static Mutex<State>> {
    STATE
        .get_or_init(|| {
            let dir = dir()?;
            let events = UnixStream::connect(dir.join(".socket2.sock"))
                .map_err(|e| eprintln!("Hyprland's events are unavailable: {e}"))
                .ok();
            if let Some(events) = events {
                thread::Builder::new()
                    .name("hyprland-events".into())
                    .spawn(move || watch(events))
                    .ok()?;
            }
            Some(Mutex::default())
        })
        .as_ref()
}

/// Binds the keys again each time Hyprland reloads its config, which drops
/// them, until Hyprland goes away.
fn watch(events: UnixStream) {
    for line in BufReader::new(events).lines() {
        let Ok(line) = line else { return };
        if line.starts_with("configreloaded>>")
            && let Some(state) = STATE.get().and_then(Option::as_ref)
        {
            let mut state = state.lock().unwrap();
            state.added.clear();
            sync(&mut state);
        }
    }
}

/// Unbinds what snapr added that isn't wanted, then binds what's missing.
fn sync(state: &mut State) {
    let (keep, gone): (Vec<_>, Vec<_>) = std::mem::take(&mut state.added)
        .into_iter()
        .partition(|b| state.wanted.contains(b));
    state.added = keep;
    for b in gone {
        // Unbinds every bind on these keys, but the only one was snapr's:
        // it wasn't added over another (see below).
        if let Err(e) = request(&format!("keyword unbind {}, {}", b.mods, b.key)) {
            eprintln!("couldn't unbind {} from Hyprland: {e}", label(&b));
        }
    }
    let State { wanted, taken, .. } = state;
    taken.retain(|b| wanted.contains(b));
    let missing: Vec<Bind> = state
        .wanted
        .iter()
        .filter(|b| !state.added.contains(b))
        .cloned()
        .collect();
    if missing.is_empty() {
        return;
    }
    let existing = match request("j/binds").and_then(|json| existing(&json)) {
        Ok(e) => e,
        Err(e) => {
            super::report(format!("Couldn't read Hyprland's key bindings: {e}"));
            return;
        }
    };
    for b in missing {
        match existing.iter().find(|e| e.is(&b)) {
            // Bound already, by the user's config or a copy of snapr that
            // didn't get to unbind it: used as it is, and left alone.
            Some(e) if e.dispatcher == "global" && e.arg == action(&b.id) => {}
            Some(_) if state.taken.contains(&b) => {}
            Some(e) => {
                super::report(format!(
                    "{} is already bound in Hyprland (to {} {}): choose other keys \
                     for that hotkey",
                    label(&b),
                    e.dispatcher,
                    e.arg
                ));
                state.taken.push(b);
            }
            None => match request(&format!(
                "keyword bind {}, {}, global, {}",
                b.mods,
                b.key,
                action(&b.id)
            )) {
                Ok(_) => state.added.push(b),
                Err(e) => super::report(format!("Hyprland couldn't bind {}: {e}", label(&b))),
            },
        }
    }
}

/// Hyprland's name for the portal's shortcut `id`, e.g. `snapr:capture`.
fn action(id: &str) -> String {
    format!("{APP_ID}:{id}")
}

/// The bind for an XDG trigger such as `CTRL+SHIFT+s`.
fn bind(id: &str, trigger: &str) -> Option<Bind> {
    let mut parts: Vec<&str> = trigger.split('+').collect();
    let key = parts.pop().filter(|k| !k.is_empty())?;
    let (mut mods, mut modmask) = (Vec::new(), 0);
    for m in parts {
        let (name, bit) = match m {
            "SHIFT" => ("SHIFT", 1),
            "CTRL" => ("CTRL", 4),
            "ALT" => ("ALT", 8),
            "LOGO" => ("SUPER", 64),
            _ => return None,
        };
        mods.push(name);
        modmask |= bit;
    }
    Some(Bind {
        id: id.into(),
        mods: mods.join(" "),
        modmask,
        key: key.into(),
    })
}

/// The keys of a bind for people, e.g. `Ctrl + Shift + S`.
fn label(b: &Bind) -> String {
    let mut keys: Vec<String> = b
        .mods
        .split(' ')
        .filter(|m| !m.is_empty())
        .map(|m| match m {
            "CTRL" => "Ctrl".into(),
            "ALT" => "Alt".into(),
            "SHIFT" => "Shift".into(),
            _ => "Super".into(),
        })
        .collect();
    keys.push(if b.key.len() == 1 { b.key.to_uppercase() } else { b.key.clone() });
    keys.join(" + ")
}

/// One of Hyprland's binds, from `hyprctl binds`.
#[derive(Deserialize)]
struct Existing {
    modmask: u32,
    key: String,
    #[serde(default)]
    submap: String,
    dispatcher: String,
    arg: String,
}

impl Existing {
    /// Whether it catches the same keys as `b`, outside any submap.
    fn is(&self, b: &Bind) -> bool {
        self.submap.is_empty() && self.modmask == b.modmask && self.key.eq_ignore_ascii_case(&b.key)
    }
}

fn existing(json: &str) -> Result<Vec<Existing>, String> {
    serde_json::from_str(json).map_err(|e| e.to_string())
}

/// The folder of the running Hyprland's sockets.
fn dir() -> Option<PathBuf> {
    let instance = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let dir = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?)
        .join("hypr")
        .join(instance);
    dir.join(".socket.sock").exists().then_some(dir)
}

/// Sends Hyprland a request, e.g. `j/binds`, and returns its answer. Commands
/// answer `ok`, or what's wrong.
fn request(command: &str) -> Result<String, String> {
    let dir = dir().ok_or("Hyprland isn't running")?;
    let mut socket = UnixStream::connect(dir.join(".socket.sock")).map_err(|e| e.to_string())?;
    socket.set_read_timeout(Some(TIMEOUT)).map_err(|e| e.to_string())?;
    socket.set_write_timeout(Some(TIMEOUT)).map_err(|e| e.to_string())?;
    socket.write_all(command.as_bytes()).map_err(|e| e.to_string())?;
    let mut answer = String::new();
    socket.read_to_string(&mut answer).map_err(|e| e.to_string())?;
    if command.starts_with("keyword ") && answer.trim() != "ok" {
        return Err(answer.trim().to_string());
    }
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binds_xdg_triggers() {
        let b = bind("capture", "CTRL+SHIFT+s").unwrap();
        assert_eq!((b.mods.as_str(), b.modmask, b.key.as_str()), ("CTRL SHIFT", 5, "s"));
        let b = bind("record", "ALT+LOGO+F9").unwrap();
        assert_eq!((b.mods.as_str(), b.modmask, b.key.as_str()), ("ALT SUPER", 72, "F9"));
        let b = bind("capture", "Print").unwrap();
        assert_eq!((b.mods.as_str(), b.modmask), ("", 0));
        assert_eq!(label(&b), "Print");
        assert_eq!(label(&bind("x", "CTRL+LOGO+4").unwrap()), "Ctrl + Super + 4");
        assert!(bind("x", "HYPER+s").is_none());
        assert!(bind("x", "CTRL+").is_none());
    }

    #[test]
    fn finds_binds_on_the_same_keys() {
        // As Hyprland 0.53 gives them, cut down.
        let json = r#"[
            {"modmask": 5, "submap": "", "key": "S", "dispatcher": "global", "arg": "snapr:capture"},
            {"modmask": 64, "submap": "", "key": "Return", "dispatcher": "exec", "arg": "kitty"},
            {"modmask": 4, "submap": "resize", "key": "r", "dispatcher": "exec", "arg": "x"}
        ]"#;
        let existing = existing(json).unwrap();
        let find = |trigger| existing.iter().position(|e| e.is(&bind("capture", trigger).unwrap()));
        assert_eq!(find("CTRL+SHIFT+s"), Some(0));
        assert_eq!(find("LOGO+Return"), Some(1));
        assert_eq!(find("CTRL+s"), None);
        // Only while that submap is on.
        assert_eq!(find("CTRL+r"), None);
    }
}
