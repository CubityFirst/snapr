//! Windows tray icon via `Shell_NotifyIconW`. The hidden window that receives
//! its messages lives on the main thread, so winit's message loop drives it.

use std::cell::RefCell;
use std::ptr::null;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIcon, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon,
    DestroyMenu, DestroyWindow, FindWindowW, GetCursorPos, GetSystemMetrics, HICON, MF_SEPARATOR,
    MF_STRING, PostMessageW, RegisterClassW, RegisterWindowMessageW, SM_CXSMICON,
    SetForegroundWindow, SetMenuDefaultItem, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
    WM_APP, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSW,
};

use super::{Callback, TrayAction, capture_label};

const CLASS_NAME: &str = "snapr.tray";
const WM_TRAY: u32 = WM_APP + 1;
/// Sent by a second instance to ask this one to show its settings.
const WM_SHOW_SETTINGS: u32 = WM_APP + 2;

const MENU: &[(usize, TrayAction)] = &[
    (1, TrayAction::Capture),
    (5, TrayAction::Recent),
    (2, TrayAction::Settings),
    (3, TrayAction::OpenFolder),
    (4, TrayAction::Quit),
];

struct State {
    hwnd: HWND,
    icon: HICON,
    tooltip: String,
    capture_label: String,
    callback: Callback,
    taskbar_created: u32,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub struct Tray {
    hwnd: HWND,
}

impl Tray {
    pub fn new(hotkey: Option<&str>, callback: Callback) -> Result<Self, String> {
        let class = wide(CLASS_NAME);
        // SAFETY: plain Win32 calls with valid, NUL-terminated strings.
        let hwnd = unsafe {
            let hinstance = GetModuleHandleW(null());
            let mut wc: WNDCLASSW = std::mem::zeroed();
            wc.lpfnWndProc = Some(wndproc);
            wc.hInstance = hinstance;
            wc.lpszClassName = class.as_ptr();
            RegisterClassW(&wc);
            // A hidden top-level window rather than a message-only one, so it
            // receives the "TaskbarCreated" broadcast after Explorer restarts.
            CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                0 as HWND,
                0 as _,
                hinstance,
                null(),
            )
        };
        if hwnd.is_null() {
            return Err("couldn't create tray window".into());
        }
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.clamp(16, 64) as u32;
        let state = State {
            hwnd,
            icon: make_icon(size),
            tooltip: tooltip(hotkey),
            capture_label: capture_label(hotkey),
            callback,
            taskbar_created: unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) },
        };
        let added = notify(&state, NIM_ADD);
        STATE.with(|s| *s.borrow_mut() = Some(state));
        if !added {
            return Err("couldn't add tray icon".into());
        }
        Ok(Self { hwnd })
    }

    pub fn set_hotkey(&self, hotkey: Option<&str>) {
        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut() {
                state.tooltip = tooltip(hotkey);
                state.capture_label = capture_label(hotkey);
                notify(state, NIM_MODIFY);
            }
        });
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        if let Some(state) = STATE.with(|s| s.borrow_mut().take()) {
            notify(&state, NIM_DELETE);
            // SAFETY: both handles were created by us and are no longer used.
            unsafe {
                DestroyIcon(state.icon);
            }
        }
        unsafe { DestroyWindow(self.hwnd) };
    }
}

/// If snapr is already running, asks it to show its settings and returns true.
pub fn signal_running_instance() -> bool {
    // SAFETY: FindWindowW/PostMessageW with a valid class name.
    unsafe {
        let hwnd = FindWindowW(wide(CLASS_NAME).as_ptr(), null());
        !hwnd.is_null() && PostMessageW(hwnd, WM_SHOW_SETTINGS, 0, 0) != 0
    }
}

fn tooltip(hotkey: Option<&str>) -> String {
    match hotkey {
        Some(h) => format!("snapr \u{2014} {h} to capture"),
        None => "snapr".into(),
    }
}

fn notify(state: &State, action: u32) -> bool {
    // SAFETY: NOTIFYICONDATAW is plain data; zeroed is a valid starting point.
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = state.hwnd;
        nid.uID = 1;
        if action != NIM_DELETE {
            nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
            nid.uCallbackMessage = WM_TRAY;
            nid.hIcon = state.icon;
            for (dst, src) in nid
                .szTip
                .iter_mut()
                .zip(state.tooltip.encode_utf16().take(127))
            {
                *dst = src;
            }
        }
        Shell_NotifyIconW(action, &nid) != 0
    }
}

fn make_icon(size: u32) -> HICON {
    let rgba = super::icon_rgba(size);
    // 32-bit icons take BGRA colour bits plus a 1-bit AND mask (unused with alpha).
    let bgra: Vec<u8> = rgba
        .chunks_exact(4)
        .flat_map(|p| [p[2], p[1], p[0], p[3]])
        .collect();
    let mask = vec![0u8; (size * size / 8) as usize];
    // SAFETY: buffers are the sizes CreateIcon expects for these dimensions.
    unsafe {
        CreateIcon(
            GetModuleHandleW(null()),
            size as i32,
            size as i32,
            1,
            32,
            mask.as_ptr(),
            bgra.as_ptr(),
        )
    }
}

fn dispatch(action: TrayAction) {
    // Clone out of the RefCell so the callback can't re-enter a borrow.
    let callback = STATE.with(|s| s.borrow().as_ref().map(|st| st.callback.clone()));
    if let Some(cb) = callback {
        cb(action);
    }
}

fn show_menu(hwnd: HWND) {
    let Some(capture) = STATE.with(|s| s.borrow().as_ref().map(|st| st.capture_label.clone()))
    else {
        return;
    };
    // SAFETY: standard popup-menu sequence on our own window.
    unsafe {
        let menu = CreatePopupMenu();
        for &(id, action) in MENU {
            let label = match action {
                TrayAction::Capture => capture.clone(),
                TrayAction::Recent => "Recent screenshots\u{2026}".into(),
                TrayAction::Settings => "Settings\u{2026}".into(),
                TrayAction::OpenFolder => "Open screenshots folder".into(),
                TrayAction::Quit => {
                    AppendMenuW(menu, MF_SEPARATOR, 0, null());
                    "Quit snapr".into()
                }
            };
            AppendMenuW(menu, MF_STRING, id, wide(&label).as_ptr());
        }
        SetMenuDefaultItem(menu, 5, 0);
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        // Needed so the menu closes when clicking elsewhere.
        SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            0,
            hwnd,
            null(),
        );
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
        if let Some(&(_, action)) = MENU.iter().find(|(id, _)| *id as i32 == cmd) {
            dispatch(action);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            match lparam as u32 & 0xffff {
                WM_LBUTTONUP => dispatch(TrayAction::Recent),
                WM_RBUTTONUP => show_menu(hwnd),
                _ => {}
            }
            0
        }
        WM_SHOW_SETTINGS => {
            dispatch(TrayAction::Recent);
            0
        }
        _ => {
            let readd = STATE.with(|s| {
                s.borrow()
                    .as_ref()
                    .filter(|st| st.taskbar_created == msg)
                    .map(|st| notify(st, NIM_ADD))
            });
            if readd.is_some() {
                return 0;
            }
            // SAFETY: forwarding unhandled messages is required.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}
