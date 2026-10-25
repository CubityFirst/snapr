//! The global capture hotkey.
//!
//! Windows uses the OS hotkey API (`RegisterHotKey`): the OS tracks the
//! modifier keys and swallows the combination so it doesn't also reach the
//! focused app. Wayland uses the desktop's global shortcuts portal. Other
//! platforms use livesplit-hotkey.

use std::sync::OnceLock;

use livesplit_hotkey::Hotkey;

use crate::settings::parse_hotkey;

#[cfg(target_os = "linux")]
mod portal;

pub type Callback = Box<dyn Fn() + Send + 'static>;

/// The capture and record hotkeys' ids (see `GlobalHotkey::new`).
pub const CAPTURE: &str = "capture";
pub const RECORD: &str = "record";

type Reporter = Box<dyn Fn(String) + Send + Sync + 'static>;

static REPORTER: OnceLock<Reporter> = OnceLock::new();
static KEYS_CHANGED: OnceLock<Box<dyn Fn() + Send + Sync + 'static>> = OnceLock::new();

/// Sets where problems found after `GlobalHotkey::set` returned go: on
/// Wayland the desktop binds hotkeys later, maybe after asking the user.
pub fn on_problem(report: impl Fn(String) + Send + Sync + 'static) {
    let _ = REPORTER.set(Box::new(report));
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn report(problem: String) {
    eprintln!("{problem}");
    if let Some(report) = REPORTER.get() {
        report(problem);
    }
}

/// Sets what's called when the keys `desktop_keys` reports change.
pub fn on_keys_changed(changed: impl Fn() + Send + Sync + 'static) {
    let _ = KEYS_CHANGED.set(Box::new(changed));
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn keys_changed() {
    if let Some(changed) = KEYS_CHANGED.get() {
        changed();
    }
}

/// Whether the desktop decides the hotkeys' keys: snapr's are only
/// suggested the first time, then changed in the desktop's settings.
pub fn desktop_picks_keys() -> bool {
    #[cfg(target_os = "linux")]
    return platform::wayland() && portal::Portal::get().is_some();
    #[cfg(not(target_os = "linux"))]
    false
}

/// How the desktop describes the keys it has bound to the hotkey `id`, in
/// its own words, e.g. "Press <Control>j", where it picks them (see
/// `desktop_picks_keys`). `None` if it has none.
pub fn desktop_keys(id: &str) -> Option<String> {
    #[cfg(target_os = "linux")]
    return portal::Portal::get()?.keys(id);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = id;
        None
    }
}

/// The key combinations in a desktop's description of a hotkey's keys (see
/// `desktop_keys`), each as its keys' names: GNOME's "Press <Control>j"
/// and KDE's "Ctrl+J" both give `[["Ctrl", "J"]]`. `None` if it can't tell.
pub fn key_combinations(description: &str) -> Option<Vec<Vec<String>>> {
    // GNOME: GTK accelerators in a sentence, e.g. "Press %s or %s".
    let gtk: Vec<_> = description.split_whitespace().filter_map(gtk_accelerator).collect();
    if !gtk.is_empty() {
        return Some(gtk);
    }
    let words: Vec<_> = description.split_whitespace().collect();
    match words[..] {
        // KDE: Qt's form, e.g. "Ctrl+Shift+S" ("Ctrl++" for the plus key).
        [keys] if keys.len() > 1 && keys.contains('+') => {
            let mut names: Vec<String> =
                keys.split('+').filter(|k| !k.is_empty()).map(String::from).collect();
            if keys.ends_with("++") {
                names.push("+".into());
            }
            Some(vec![names])
        }
        // GNOME, a key on its own: "Press Print".
        [.., last] if words.len() > 1 => known_key(last).map(|k| vec![vec![k]]),
        _ => None,
    }
}

/// A GTK accelerator's keys, e.g. `<Control><Shift>s` → Ctrl, Shift, S.
/// Only with modifiers: a bare word could be any word.
fn gtk_accelerator(word: &str) -> Option<Vec<String>> {
    let mut rest = word.strip_suffix([',', '.']).unwrap_or(word);
    if !rest.starts_with('<') {
        return None;
    }
    let mut keys = Vec::new();
    while let Some(after) = rest.strip_prefix('<') {
        let (modifier, tail) = after.split_once('>')?;
        keys.push(
            match modifier {
                "Control" | "Ctrl" | "Primary" => "Ctrl",
                "Alt" | "Mod1" => "Alt",
                "Super" | "Mod4" => "Super",
                "Mod2" => "Num Lock",
                m => m,
            }
            .to_string(),
        );
        rest = tail;
    }
    if rest.is_empty() {
        return None;
    }
    keys.push(known_key(rest).unwrap_or_else(|| rest.replace('_', " ")));
    Some(keys)
}

/// A key's name to show for an XKB keysym name (`Page_Up` → Page Up, `j`
/// → J), if it's one snapr knows.
fn known_key(keysym: &str) -> Option<String> {
    let mut chars = keysym.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Some(c.to_uppercase().collect());
    }
    if keysym.strip_prefix('F').and_then(|n| n.parse::<u8>().ok()).is_some_and(|n| (1..=24).contains(&n)) {
        return Some(keysym.into());
    }
    if let Some(d) = keysym.strip_prefix("KP_").filter(|d| d.len() == 1) {
        return Some(format!("Num {d}"));
    }
    let name = match keysym {
        "Print" => "Print Screen",
        "Pause" => "Pause",
        "Scroll_Lock" => "Scroll Lock",
        "space" => "Space",
        "Return" => "Enter",
        "Tab" => "Tab",
        "Escape" => "Esc",
        "BackSpace" => "Backspace",
        "Insert" => "Insert",
        "Delete" => "Delete",
        "Home" => "Home",
        "End" => "End",
        "Page_Up" => "Page Up",
        "Page_Down" => "Page Down",
        "Up" => "Up",
        "Down" => "Down",
        "Left" => "Left",
        "Right" => "Right",
        "minus" => "-",
        "equal" => "=",
        "bracketleft" => "[",
        "bracketright" => "]",
        "backslash" => "\\",
        "semicolon" => ";",
        "apostrophe" => "'",
        "grave" => "`",
        "comma" => ",",
        "period" => ".",
        "slash" => "/",
        "KP_Add" => "Num +",
        "KP_Subtract" => "Num -",
        "KP_Multiply" => "Num *",
        "KP_Divide" => "Num /",
        "KP_Decimal" => "Num .",
        _ => return None,
    };
    Some(name.into())
}

/// Whether the desktop is still binding the hotkeys, where it picks their
/// keys: GNOME asks the user about new ones first, and none work meanwhile.
pub fn desktop_asking() -> bool {
    #[cfg(target_os = "linux")]
    return portal::Portal::get().is_some_and(|p| p.binding());
    #[cfg(not(target_os = "linux"))]
    false
}

/// Opens the desktop's settings where snapr's hotkeys' keys are changed.
pub fn open_desktop_settings() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let portal = portal::Portal::get().ok_or("the desktop doesn't keep snapr's hotkeys")?;
        if portal.configure().is_ok() {
            return Ok(());
        }
        // GNOME's portal can't open them itself: Settings' Apps page on
        // snapr has a Global Shortcuts button.
        if crate::portal::gnome() {
            return std::process::Command::new("gnome-control-center")
                .args(["applications", "snapr.desktop"])
                .spawn()
                .map(|mut settings| {
                    // Collected when it exits, so it doesn't linger as a
                    // zombie. If Settings is already open this one hands
                    // over to it and exits straight away.
                    std::thread::spawn(move || settings.wait());
                })
                .map_err(|e| format!("couldn't open Settings: {e}"));
        }
    }
    Err("open your desktop's keyboard shortcut settings to change them".into())
}

pub struct GlobalHotkey {
    inner: platform::Registrar,
    current: Option<Hotkey>,
    /// Registered, with `current` or (offered to the desktop) without keys.
    active: bool,
}

impl GlobalHotkey {
    /// `id` and `description` name the hotkey to desktops that keep a list
    /// of each app's shortcuts (Wayland); the id has to stay the same from
    /// run to run.
    pub fn new(id: &str, description: &str, callback: Callback) -> Result<Self, String> {
        Ok(Self {
            inner: platform::Registrar::new(id, description, callback)?,
            current: None,
            active: false,
        })
    }

    pub fn current(&self) -> Option<Hotkey> {
        self.current
    }

    /// Unregisters the hotkey, if any.
    pub fn clear(&mut self) {
        self.current = None;
        if std::mem::take(&mut self.active) {
            self.inner.unregister();
        }
    }

    /// Replaces the registered hotkey.
    pub fn set(&mut self, spec: &str) -> Result<(), String> {
        let hotkey = parse_hotkey(spec)?;
        if self.active && self.current == Some(hotkey) {
            return Ok(());
        }
        let result = self.inner.register(hotkey);
        self.current = result.is_ok().then_some(hotkey);
        self.active = result.is_ok();
        result
    }

    /// Registers the hotkey without keys, for the desktop to ask for some.
    /// Only where it picks them (see `desktop_picks_keys`).
    pub fn offer(&mut self) -> Result<(), String> {
        #[cfg(target_os = "linux")]
        if let platform::Registrar::Portal(s) = &self.inner {
            s.offer();
            self.current = None;
            self.active = true;
            return Ok(());
        }
        Err("press Record, then the key combination".into())
    }
}

#[cfg(test)]
mod key_tests {
    use super::key_combinations;

    fn keys(description: &str) -> Option<Vec<Vec<String>>> {
        key_combinations(description)
    }

    #[test]
    fn reads_gnome_descriptions() {
        assert_eq!(keys("Press <Control>j"), Some(vec![vec!["Ctrl".into(), "J".into()]]));
        assert_eq!(
            keys("Press <Shift><Alt>v or <Super>Page_Up"),
            Some(vec![
                vec!["Shift".into(), "Alt".into(), "V".into()],
                vec!["Super".into(), "Page Up".into()],
            ])
        );
        assert_eq!(keys("Press Print"), Some(vec![vec!["Print Screen".into()]]));
        assert_eq!(keys("Drücken Sie <Control>KP_7"), Some(vec![vec!["Ctrl".into(), "Num 7".into()]]));
    }

    #[test]
    fn reads_kde_descriptions() {
        assert_eq!(keys("Ctrl+Shift+S"), Some(vec![vec!["Ctrl".into(), "Shift".into(), "S".into()]]));
        assert_eq!(keys("Ctrl++"), Some(vec![vec!["Ctrl".into(), "+".into()]]));
    }

    #[test]
    fn leaves_other_text_alone() {
        assert_eq!(keys("Press something"), None);
        assert_eq!(keys(""), None);
    }
}

#[cfg(windows)]
mod platform {
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::thread;

    use livesplit_hotkey::{Hotkey, Modifiers};
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_APP, WM_HOTKEY,
        WM_QUIT, WM_USER,
    };

    const HOTKEY_ID: i32 = 1;
    /// Asks the hotkey thread to (re-)register the pending hotkey.
    const WM_REGISTER: u32 = WM_APP + 10;
    const ERROR_HOTKEY_ALREADY_REGISTERED: u32 = 1409;

    type Request = Option<(HOT_KEY_MODIFIERS, u32)>;

    /// Hotkeys registered with a NULL window belong to the registering thread,
    /// so a small thread owns the registration and runs its message loop.
    pub struct Registrar {
        thread_id: u32,
        request: Arc<Mutex<Request>>,
        replies: Receiver<Result<(), String>>,
    }

    impl Registrar {
        pub fn new(_id: &str, _description: &str, callback: super::Callback) -> Result<Self, String> {
            let request: Arc<Mutex<Request>> = Arc::default();
            let (reply_tx, replies) = mpsc::channel();
            let (id_tx, id_rx) = mpsc::channel();
            let pending = request.clone();
            thread::Builder::new()
                .name("hotkey".into())
                .spawn(move || run(callback, pending, reply_tx, id_tx))
                .map_err(|e| format!("couldn't start hotkey thread: {e}"))?;
            let thread_id = id_rx
                .recv()
                .map_err(|_| "hotkey thread failed to start".to_string())?;
            Ok(Self {
                thread_id,
                request,
                replies,
            })
        }

        pub fn register(&self, hotkey: Hotkey) -> Result<(), String> {
            let vk = virtual_key(hotkey.key_code.name()).ok_or_else(|| {
                format!(
                    "{} can't be used as a hotkey on Windows",
                    hotkey.key_code.name()
                )
            })?;
            let mut mods = MOD_NOREPEAT;
            for (m, flag) in [
                (Modifiers::CONTROL, MOD_CONTROL),
                (Modifiers::ALT, MOD_ALT),
                (Modifiers::SHIFT, MOD_SHIFT),
                (Modifiers::META, MOD_WIN),
            ] {
                if hotkey.modifiers.contains(m) {
                    mods |= flag;
                }
            }
            *self.request.lock().unwrap() = Some((mods, vk));
            // SAFETY: posting a message to our own thread.
            if unsafe { PostThreadMessageW(self.thread_id, WM_REGISTER, 0, 0) } == 0 {
                return Err("hotkey thread isn't running".into());
            }
            self.replies
                .recv()
                .map_err(|_| "hotkey thread stopped".to_string())?
                .map_err(|e| format!("{hotkey}: {e}"))
        }
    }

    impl Registrar {
        pub fn unregister(&self) {
            // An empty request just unregisters.
            *self.request.lock().unwrap() = None;
            // SAFETY: posting a message to our own thread.
            if unsafe { PostThreadMessageW(self.thread_id, WM_REGISTER, 0, 0) } != 0 {
                let _ = self.replies.recv();
            }
        }
    }

    impl Drop for Registrar {
        fn drop(&mut self) {
            // SAFETY: posting a message to our own thread.
            unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
        }
    }

    fn run(
        callback: super::Callback,
        pending: Arc<Mutex<Request>>,
        replies: Sender<Result<(), String>>,
        id: Sender<u32>,
    ) {
        // SAFETY: plain Win32 calls on this thread's own message queue.
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            // Create the message queue before anyone posts to it.
            PeekMessageW(
                &mut msg,
                std::ptr::null_mut(),
                WM_USER,
                WM_USER,
                PM_NOREMOVE,
            );
            let _ = id.send(GetCurrentThreadId());
            let mut registered = false;
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                match msg.message {
                    WM_HOTKEY if msg.wParam == HOTKEY_ID as usize => callback(),
                    WM_REGISTER => {
                        if registered {
                            UnregisterHotKey(std::ptr::null_mut(), HOTKEY_ID);
                            registered = false;
                        }
                        let result = match pending.lock().unwrap().take() {
                            None => Ok(()),
                            Some((mods, vk)) => {
                                if RegisterHotKey(std::ptr::null_mut(), HOTKEY_ID, mods, vk) != 0 {
                                    registered = true;
                                    Ok(())
                                } else if GetLastError() == ERROR_HOTKEY_ALREADY_REGISTERED {
                                    Err("already in use by another app".to_string())
                                } else {
                                    Err(format!("couldn't register (error {})", GetLastError()))
                                }
                            }
                        };
                        let _ = replies.send(result);
                    }
                    _ => {}
                }
            }
            if registered {
                UnregisterHotKey(std::ptr::null_mut(), HOTKEY_ID);
            }
        }
    }

    /// Windows virtual-key code for a key name such as `KeyS` or `F5`.
    fn virtual_key(name: &str) -> Option<u32> {
        if let Some(c) = name.strip_prefix("Key").filter(|c| c.len() == 1) {
            return Some(c.as_bytes()[0] as u32); // VK_A..VK_Z are 'A'..'Z'
        }
        if let Some(d) = name.strip_prefix("Digit").filter(|d| d.len() == 1) {
            return Some(d.as_bytes()[0] as u32); // VK_0..VK_9 are '0'..'9'
        }
        if let Some(d) = name
            .strip_prefix("Numpad")
            .and_then(|d| d.parse::<u32>().ok())
        {
            return (d <= 9).then(|| VK_NUMPAD0 as u32 + d);
        }
        if let Some(n) = name.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()) {
            return (1..=24).contains(&n).then(|| VK_F1 as u32 + n - 1);
        }
        let vk = match name {
            "PrintScreen" => VK_SNAPSHOT,
            "Pause" => VK_PAUSE,
            "ScrollLock" => VK_SCROLL,
            "Space" => VK_SPACE,
            "Enter" => VK_RETURN,
            "Tab" => VK_TAB,
            "Escape" => VK_ESCAPE,
            "Backspace" => VK_BACK,
            "Insert" => VK_INSERT,
            "Delete" => VK_DELETE,
            "Home" => VK_HOME,
            "End" => VK_END,
            "PageUp" => VK_PRIOR,
            "PageDown" => VK_NEXT,
            "ArrowUp" => VK_UP,
            "ArrowDown" => VK_DOWN,
            "ArrowLeft" => VK_LEFT,
            "ArrowRight" => VK_RIGHT,
            "Minus" => VK_OEM_MINUS,
            "Equal" => VK_OEM_PLUS,
            "BracketLeft" => VK_OEM_4,
            "BracketRight" => VK_OEM_6,
            "Backslash" => VK_OEM_5,
            "Semicolon" => VK_OEM_1,
            "Quote" => VK_OEM_7,
            "Backquote" => VK_OEM_3,
            "Comma" => VK_OEM_COMMA,
            "Period" => VK_OEM_PERIOD,
            "Slash" => VK_OEM_2,
            "NumpadAdd" => VK_ADD,
            "NumpadSubtract" => VK_SUBTRACT,
            "NumpadMultiply" => VK_MULTIPLY,
            "NumpadDivide" => VK_DIVIDE,
            "NumpadDecimal" => VK_DECIMAL,
            _ => return None,
        };
        Some(vk as u32)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn maps_common_keys() {
            assert_eq!(virtual_key("KeyH"), Some('H' as u32));
            assert_eq!(virtual_key("Digit4"), Some('4' as u32));
            assert_eq!(virtual_key("F12"), Some(VK_F12 as u32));
            assert_eq!(virtual_key("PrintScreen"), Some(VK_SNAPSHOT as u32));
            assert_eq!(virtual_key("KeyHome"), None);
        }

        /// Registers a real hotkey and checks it fires (and that a plain key
        /// press doesn't). Sends synthetic input, so it's opt-in:
        /// `cargo test hotkey_fires -- --ignored`.
        #[test]
        #[ignore]
        fn hotkey_fires_only_with_modifiers() {
            use std::time::Duration;
            let (tx, rx) = mpsc::channel();
            let reg = Registrar::new(Box::new(move || {
                let _ = tx.send(());
            }))
            .unwrap();
            let hotkey: Hotkey = "Ctrl + Alt + Shift + F24".parse().unwrap();
            reg.register(hotkey).unwrap();
            let press = |vk: VIRTUAL_KEY, up: bool| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: vk,
                        wScan: 0,
                        dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
            let send = |keys: &[INPUT]| unsafe {
                SendInput(keys.len() as u32, keys.as_ptr(), size_of::<INPUT>() as i32)
            };
            send(&[press(VK_F24, false), press(VK_F24, true)]);
            assert!(
                rx.recv_timeout(Duration::from_millis(300)).is_err(),
                "fired without modifiers"
            );
            send(&[
                press(VK_CONTROL, false),
                press(VK_MENU, false),
                press(VK_SHIFT, false),
                press(VK_F24, false),
                press(VK_F24, true),
                press(VK_SHIFT, true),
                press(VK_MENU, true),
                press(VK_CONTROL, true),
            ]);
            assert!(
                rx.recv_timeout(Duration::from_secs(2)).is_ok(),
                "didn't fire with modifiers"
            );
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use livesplit_hotkey::{ConsumePreference, Hook, Hotkey};
    use std::sync::Arc;

    pub enum Registrar {
        Hook(HookRegistrar),
        #[cfg(target_os = "linux")]
        Portal(super::portal::Shortcut),
    }

    impl Registrar {
        pub fn new(id: &str, description: &str, callback: super::Callback) -> Result<Self, String> {
            // An X11 grab would work under Wayland too, through XWayland,
            // but only while an X11 app has the focus. Reading the keyboard
            // devices (evdev) works, but needs the input group and doesn't
            // see remote desktop input.
            #[cfg(target_os = "linux")]
            if wayland() {
                if let Some(portal) = super::portal::Portal::get() {
                    return Ok(Self::Portal(portal.add(id, description, callback)));
                }
                return HookRegistrar::new(ConsumePreference::MustNotConsume, callback)
                    .map(Self::Hook)
                    .map_err(|_| {
                        "global hotkeys on Wayland need the desktop's global shortcuts \
                         portal (GNOME 48 or later, KDE Plasma) or the input group"
                            .to_string()
                    });
            }
            let _ = (id, description);
            HookRegistrar::new(ConsumePreference::PreferConsume, callback).map(Self::Hook)
        }

        pub fn register(&self, hotkey: Hotkey) -> Result<(), String> {
            match self {
                Self::Hook(h) => h.register(hotkey),
                #[cfg(target_os = "linux")]
                Self::Portal(s) => s.register(hotkey),
            }
        }

        pub fn unregister(&self) {
            match self {
                Self::Hook(h) => h.unregister(),
                #[cfg(target_os = "linux")]
                Self::Portal(s) => s.unregister(),
            }
        }
    }

    #[cfg(target_os = "linux")]
    pub fn wayland() -> bool {
        std::env::var_os("WAYLAND_DISPLAY").is_some()
    }

    pub struct HookRegistrar {
        hook: Hook,
        callback: Arc<dyn Fn() + Send + Sync>,
        current: std::cell::Cell<Option<Hotkey>>,
    }

    impl HookRegistrar {
        fn new(consume: ConsumePreference, callback: super::Callback) -> Result<Self, String> {
            let hook = Hook::with_consume_preference(consume)
                .map_err(|e| unavailable(&e.to_string()))?;
            let callback = std::sync::Mutex::new(callback);
            Ok(Self {
                hook,
                callback: Arc::new(move || (callback.lock().unwrap())()),
                current: Default::default(),
            })
        }

        fn register(&self, hotkey: Hotkey) -> Result<(), String> {
            if let Some(old) = self.current.take() {
                let _ = self.hook.unregister(old);
            }
            let callback = self.callback.clone();
            self.hook
                .register(hotkey, move || callback())
                .map_err(|e| format!("couldn't register hotkey {hotkey}: {e}"))?;
            self.current.set(Some(hotkey));
            Ok(())
        }

        fn unregister(&self) {
            if let Some(old) = self.current.take() {
                let _ = self.hook.unregister(old);
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn unavailable(e: &str) -> String {
        format!("global hotkeys unavailable: {e}")
    }

    /// Why the keyboard can't be watched. On macOS that's usually a missing
    /// Accessibility permission, which macOS is asked (once a run) to offer.
    #[cfg(target_os = "macos")]
    fn unavailable(e: &str) -> String {
        static TRUSTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *TRUSTED.get_or_init(ask_for_accessibility) {
            format!("global hotkeys unavailable: {e}")
        } else {
            crate::macos::ACCESSIBILITY_OFF.into()
        }
    }

    /// Whether snapr has Accessibility permission. If not, macOS shows its
    /// prompt that leads to the setting.
    #[cfg(target_os = "macos")]
    fn ask_for_accessibility() -> bool {
        use objc2_foundation::{NSDictionary, NSNumber, NSString};

        #[link(name = "ApplicationServices", kind = "framework")]
        unsafe extern "C" {
            fn AXIsProcessTrustedWithOptions(options: *const std::ffi::c_void) -> u8;
        }
        // kAXTrustedCheckOptionPrompt's value.
        let key = NSString::from_str("AXTrustedCheckOptionPrompt");
        let options = NSDictionary::from_slices(&[&*key], &[&*NSNumber::new_bool(true)]);
        // SAFETY: an NSDictionary is a CFDictionary (toll-free bridged), and
        // it outlives the call.
        unsafe { AXIsProcessTrustedWithOptions(objc2::rc::Retained::as_ptr(&options).cast()) != 0 }
    }
}
