//! System tray icon, implemented directly on each platform's native API.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::Tray;
#[cfg(target_os = "macos")]
pub use macos::Tray;
#[cfg(windows)]
pub use windows::{Tray, signal_running_instance};

use std::sync::Arc;

use crate::settings::ToolAction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Capture,
    /// Start one of the tools, from the Tools submenu.
    Tool(ToolAction),
    /// Open the main window on the Recent page.
    Recent,
    Settings,
    OpenFolder,
    Quit,
}

pub type Callback = Arc<dyn Fn(TrayAction) + Send + Sync>;

fn capture_label(hotkey: Option<&str>) -> String {
    match hotkey {
        Some(h) => format!("Capture region    {h}"),
        None => "Capture region".into(),
    }
}

/// The app icon as RGBA: blue viewfinder corners around a small dot.
pub fn icon_rgba(size: u32) -> Vec<u8> {
    let s = size as f32;
    let margin = s * 0.08;
    let thick = (s * 0.11).max(1.5);
    let arm = s * 0.34;
    let dot = s * 0.12;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // Distance from the nearest edge, mirrored so all four corners match.
            let dx = fx.min(s - fx) - margin;
            let dy = fy.min(s - fy) - margin;
            let corner =
                dx >= 0.0 && dy >= 0.0 && ((dx < thick && dy < arm) || (dy < thick && dx < arm));
            let center = (fx - s / 2.0).abs() < dot && (fy - s / 2.0).abs() < dot;
            rgba.extend_from_slice(&if corner || center {
                [0x3d, 0x9b, 0xff, 0xff]
            } else {
                [0, 0, 0, 0]
            });
        }
    }
    rgba
}
