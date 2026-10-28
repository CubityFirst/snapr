//! Windows tray icon via `Shell_NotifyIconW`. The hidden window that receives
//! its messages lives on the main thread, so winit's message loop drives it.

use std::cell::RefCell;
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::ptr::null;

use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::DataExchange::COPYDATASTRUCT;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIcon, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon,
    DestroyMenu, DestroyWindow, FindWindowW, GetCursorPos, GetSystemMetrics, HICON, MF_POPUP,
    MF_SEPARATOR, MF_STRING, PostMessageW, RegisterClassW, RegisterWindowMessageW, SM_CXSMICON,
    SendMessageW, SetForegroundWindow, SetMenuDefaultItem, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenu, WM_APP, WM_COPYDATA, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WM_SETTINGCHANGE,
    WNDCLASSW,
};

use super::{Callback, TrayAction, capture_label};
use crate::settings::{ToolAction, TrayClick, TrayIconStyle};

const CLASS_NAME: &str = "snapr.tray";
const WM_TRAY: u32 = WM_APP + 1;
/// Sent by a second instance to ask this one to show its settings.
const WM_SHOW_SETTINGS: u32 = WM_APP + 2;
/// Sent by `snapr --uninstall` to ask this one to quit.
const WM_QUIT_SNAPR: u32 = WM_APP + 3;
/// Marks a `WM_COPYDATA` from `snapr --upload` (Explorer's "Upload with
/// Snapr"): files to upload, NUL-separated UTF-16.
const UPLOAD_FILES: usize = 0x736e_7570;

const MENU: &[(usize, TrayAction)] = &[
    (1, TrayAction::Capture),
    (5, TrayAction::Recent),
    (2, TrayAction::Settings),
    (3, TrayAction::OpenFolder),
    (4, TrayAction::Quit),
];
/// The Tools submenu's items take IDs from here, in `ToolAction::ALL` order.
const TOOL_ID: usize = 100;

struct State {
    hwnd: HWND,
    icon: HICON,
    size: u32,
    tooltip: String,
    /// How far the uploads have got, while there are any.
    progress: Option<super::Progress>,
    style: TrayIconStyle,
    /// What `style` came to when the icon was last drawn.
    color: [u8; 4],
    capture_label: String,
    callback: Callback,
    taskbar_created: u32,
    /// What a left-click does.
    click: TrayClick,
    /// Gets the files another instance was asked to upload.
    on_files: Option<FilesCallback>,
}

pub type FilesCallback = std::sync::Arc<dyn Fn(Vec<PathBuf>) + Send + Sync>;

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
        let style = TrayIconStyle::default();
        let color = super::color(style);
        let state = State {
            hwnd,
            icon: make_icon(size, None, color),
            size,
            tooltip: tooltip(hotkey),
            progress: None,
            style,
            color,
            capture_label: capture_label(hotkey),
            callback,
            taskbar_created: unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) },
            click: TrayClick::default(),
            on_files: None,
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

    pub fn on_files(&self, callback: FilesCallback) {
        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut() {
                state.on_files = Some(callback);
            }
        });
    }

    pub fn set_click(&self, click: TrayClick) {
        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut() {
                state.click = click;
            }
        });
    }

    /// Shows how far the uploads have got on the icon, or the plain icon
    /// with `None`.
    pub fn set_progress(&self, progress: Option<super::Progress>) {
        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut()
                && state.progress != progress
            {
                state.progress = progress;
                redraw(state);
            }
        });
    }

    pub fn set_style(&self, style: TrayIconStyle) {
        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut() {
                state.style = style;
                recolor(state);
            }
        });
    }
}

/// Redraws the icon if its style now comes to another colour: the taskbar's
/// turned light or dark.
fn recolor(state: &mut State) {
    let color = super::color(state.style);
    if color != state.color {
        state.color = color;
        redraw(state);
    }
}

fn redraw(state: &mut State) {
    let old = state.icon;
    state.icon = make_icon(state.size, state.progress, state.color);
    notify(state, NIM_MODIFY);
    // SAFETY: ours, and the shell has been given the new one.
    unsafe { DestroyIcon(old) };
}

/// Whether the taskbar is light, from the "Choose your default Windows mode"
/// setting. Dark if it can't be read, as it is on Windows 10 before that
/// setting existed.
pub(super) fn light_taskbar() -> bool {
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let value = wide("SystemUsesLightTheme");
    let mut dword = 0u32;
    let mut len = 4u32;
    // SAFETY: the strings are NUL-terminated and outlive the call; `len` is
    // the size of `dword` in bytes.
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&raw mut dword).cast(),
            &mut len,
        )
    };
    err == ERROR_SUCCESS && dword != 0
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

/// Hands files to a running snapr to upload. Returns whether one took them.
pub fn send_files(paths: &[PathBuf]) -> bool {
    let data: Vec<u16> = paths
        .iter()
        .flat_map(|p| p.as_os_str().encode_wide().chain(Some(0)))
        .collect();
    let copy = COPYDATASTRUCT {
        dwData: UPLOAD_FILES,
        cbData: (data.len() * 2) as u32,
        lpData: data.as_ptr() as *mut _,
    };
    // SAFETY: `copy` and its data outlive the call, which returns once the
    // other instance has read them.
    unsafe {
        let hwnd = FindWindowW(wide(CLASS_NAME).as_ptr(), null());
        !hwnd.is_null() && SendMessageW(hwnd, WM_COPYDATA, 0, &raw const copy as LPARAM) != 0
    }
}

/// Asks a running snapr to quit, as from its tray menu. Returns whether one
/// was asked.
pub fn quit_running_instance() -> bool {
    // SAFETY: as above.
    unsafe {
        let hwnd = FindWindowW(wide(CLASS_NAME).as_ptr(), null());
        !hwnd.is_null() && PostMessageW(hwnd, WM_QUIT_SNAPR, 0, 0) != 0
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
            let tip = match state.progress {
                Some(p) => format!("snapr \u{2014} uploading, {}% done", p.percent),
                None => state.tooltip.clone(),
            };
            for (dst, src) in nid.szTip.iter_mut().zip(tip.encode_utf16().take(127))
            {
                *dst = src;
            }
        }
        Shell_NotifyIconW(action, &nid) != 0
    }
}

fn make_icon(size: u32, progress: Option<super::Progress>, color: [u8; 4]) -> HICON {
    let rgba = super::icon_with_progress(size, progress, color);
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

fn click() -> TrayClick {
    STATE.with(|s| s.borrow().as_ref().map_or_else(TrayClick::default, |st| st.click))
}

fn show_menu(hwnd: HWND) {
    let Some(capture) = STATE.with(|s| s.borrow().as_ref().map(|st| st.capture_label.clone()))
    else {
        return;
    };
    // In bold: what a left-click does, if it's on the menu.
    let default = match click() {
        TrayClick::Open => Some(5),
        TrayClick::Capture => Some(1),
        _ => None,
    };
    // SAFETY: standard popup-menu sequence on our own window.
    unsafe {
        let menu = CreatePopupMenu();
        for &(id, action) in MENU {
            let label = match action {
                TrayAction::Capture => capture.clone(),
                TrayAction::Click | TrayAction::Tool(_) => unreachable!(),
                TrayAction::Recent => "Recent screenshots\u{2026}".into(),
                TrayAction::Settings => "Settings\u{2026}".into(),
                TrayAction::OpenFolder => "Open screenshots folder".into(),
                TrayAction::Quit => {
                    AppendMenuW(menu, MF_SEPARATOR, 0, null());
                    "Quit snapr".into()
                }
            };
            AppendMenuW(menu, MF_STRING, id, wide(&label).as_ptr());
            if action == TrayAction::Capture {
                // Destroyed along with `menu`.
                let tools = CreatePopupMenu();
                for (i, tool) in ToolAction::ALL.into_iter().enumerate() {
                    AppendMenuW(tools, MF_STRING, TOOL_ID + i, wide(tool.label()).as_ptr());
                }
                AppendMenuW(menu, MF_POPUP, tools as usize, wide("Tools").as_ptr());
            }
        }
        if let Some(id) = default {
            SetMenuDefaultItem(menu, id, 0);
        }
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
        let tool = (cmd as usize)
            .checked_sub(TOOL_ID)
            .and_then(|i| ToolAction::ALL.get(i));
        if let Some(&tool) = tool {
            dispatch(TrayAction::Tool(tool));
        } else if let Some(&(_, action)) = MENU.iter().find(|(id, _)| *id as i32 == cmd) {
            dispatch(action);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            match lparam as u32 & 0xffff {
                WM_LBUTTONUP if click() == TrayClick::Menu => show_menu(hwnd),
                WM_LBUTTONUP => dispatch(TrayAction::Click),
                WM_RBUTTONUP => show_menu(hwnd),
                _ => {}
            }
            0
        }
        WM_SHOW_SETTINGS => {
            dispatch(TrayAction::Recent);
            0
        }
        WM_QUIT_SNAPR => {
            dispatch(TrayAction::Quit);
            0
        }
        WM_COPYDATA => {
            // SAFETY: for WM_COPYDATA, `lparam` points to a COPYDATASTRUCT
            // whose data is readable for the duration of the message.
            let copy = unsafe { &*(lparam as *const COPYDATASTRUCT) };
            let callback = STATE.with(|s| s.borrow().as_ref().and_then(|st| st.on_files.clone()));
            let Some(callback) = callback.filter(|_| copy.dwData == UPLOAD_FILES && !copy.lpData.is_null()) else {
                return 0;
            };
            // SAFETY: as above; `cbData` is its length in bytes.
            let data = unsafe {
                std::slice::from_raw_parts(copy.lpData as *const u16, copy.cbData as usize / 2)
            };
            let paths = data
                .split(|&c| c == 0)
                .filter(|p| !p.is_empty())
                .map(|p| PathBuf::from(OsString::from_wide(p)))
                .collect();
            callback(paths);
            1
        }
        // Among other things, the light or dark mode was changed.
        WM_SETTINGCHANGE => {
            STATE.with(|s| s.borrow_mut().as_mut().map(recolor));
            // SAFETY: as below.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
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
