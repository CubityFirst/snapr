//! The global capture hotkey.
//!
//! Windows uses the OS hotkey API (`RegisterHotKey`): the OS tracks the
//! modifier keys and swallows the combination so it doesn't also reach the
//! focused app. Other platforms use livesplit-hotkey.

use livesplit_hotkey::Hotkey;

use crate::settings::parse_hotkey;

pub type Callback = Box<dyn Fn() + Send + 'static>;

pub struct GlobalHotkey {
    inner: platform::Registrar,
    current: Option<Hotkey>,
}

impl GlobalHotkey {
    pub fn new(callback: Callback) -> Result<Self, String> {
        Ok(Self {
            inner: platform::Registrar::new(callback)?,
            current: None,
        })
    }

    pub fn current(&self) -> Option<Hotkey> {
        self.current
    }

    /// Unregisters the hotkey, if any.
    pub fn clear(&mut self) {
        if self.current.take().is_some() {
            self.inner.unregister();
        }
    }

    /// Replaces the registered hotkey.
    pub fn set(&mut self, spec: &str) -> Result<(), String> {
        let hotkey = parse_hotkey(spec)?;
        if self.current == Some(hotkey) {
            return Ok(());
        }
        let result = self.inner.register(hotkey);
        self.current = result.is_ok().then_some(hotkey);
        result
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
        pub fn new(callback: super::Callback) -> Result<Self, String> {
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

    pub struct Registrar {
        hook: Hook,
        callback: Arc<dyn Fn() + Send + Sync>,
        current: std::cell::Cell<Option<Hotkey>>,
    }

    impl Registrar {
        pub fn new(callback: super::Callback) -> Result<Self, String> {
            let hook = Hook::with_consume_preference(ConsumePreference::PreferConsume)
                .map_err(|e| format!("global hotkeys unavailable: {e}"))?;
            let callback = std::sync::Mutex::new(callback);
            Ok(Self {
                hook,
                callback: Arc::new(move || (callback.lock().unwrap())()),
                current: Default::default(),
            })
        }

        pub fn register(&self, hotkey: Hotkey) -> Result<(), String> {
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

        pub fn unregister(&self) {
            if let Some(old) = self.current.take() {
                let _ = self.hook.unregister(old);
            }
        }
    }
}
