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
pub use windows::{Tray, quit_running_instance, signal_running_instance};

use std::sync::Arc;

use crate::settings::{ToolAction, TrayIconStyle};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    /// The icon was left-clicked: whatever `Settings::tray_click` says.
    Click,
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

/// What the tray icon shows while uploading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// How much is done, from 0 to 100.
    pub percent: u8,
    /// Show the percentage above the bar instead of the viewfinder.
    pub number: bool,
}

const BLUE: [u8; 4] = [0x3d, 0x9b, 0xff, 0xff];
const WHITE: [u8; 4] = [0xff, 0xff, 0xff, 0xff];
const BLACK: [u8; 4] = [0, 0, 0, 0xff];
const CLEAR: [u8; 4] = [0, 0, 0, 0];

/// Digits 3 pixels wide and 5 high, a row of 3 bits at a time, top first.
const DIGITS: [u16; 10] = [
    0b111_101_101_101_111,
    0b010_110_010_010_111,
    0b111_001_111_100_111,
    0b111_001_111_001_111,
    0b101_101_111_001_001,
    0b111_100_111_001_111,
    0b111_100_111_101_111,
    0b111_001_001_001_001,
    0b111_101_111_101_111,
    0b111_101_111_001_111,
];

/// The app icon as RGBA: blue viewfinder corners around a small dot.
pub fn icon_rgba(size: u32) -> Vec<u8> {
    icon_with_progress(size, None, BLUE)
}

/// The colour `style` draws the tray icon in, as things are now.
// macOS draws it as a template image, tinted to suit the menu bar.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn color(style: TrayIconStyle) -> [u8; 4] {
    // Taskbars are dark unless the system says otherwise.
    #[cfg(windows)]
    let light = style == TrayIconStyle::Taskbar && windows::light_taskbar();
    #[cfg(not(windows))]
    let light = false;
    color_on(style, light)
}

/// The colour `style` draws the tray icon in on a light or dark taskbar.
pub fn color_on(style: TrayIconStyle, light: bool) -> [u8; 4] {
    match style {
        TrayIconStyle::Blue => BLUE,
        TrayIconStyle::White => WHITE,
        TrayIconStyle::Black => BLACK,
        TrayIconStyle::Taskbar if light => BLACK,
        TrayIconStyle::Taskbar => WHITE,
    }
}

/// The app icon, or with `progress` a bar across its bottom showing how far
/// the uploads have got: an outline filled from the left with nothing behind
/// it, so it reads on light and dark taskbars and as a macOS template image.
/// Above it is the top of the viewfinder, or the percentage. All of it is
/// drawn in `color`.
pub fn icon_with_progress(size: u32, progress: Option<Progress>, color: [u8; 4]) -> Vec<u8> {
    let Some(progress) = progress else {
        return pixels(size, color, |x, y| viewfinder(size, x, y));
    };
    // The bar's edges, in whole pixels so it stays crisp at 16px.
    let line = (size / 16).max(1);
    let percent = progress.percent.min(100);
    let digits = Digits::new(size, percent);
    let top = if progress.number {
        // Below the digits, with a gap.
        digits.y0 + digits.height + line
    } else {
        size - (size * 3).div_ceil(8)
    };
    let (left, right) = (size / 16, size - size / 16);
    let done = f32::from(percent) / 100.0;
    let fill = (left + line) as f32 + done * (right - left - 2 * line) as f32;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let alpha = if y + line < top {
                // Above the bar, with a clear row between them.
                let on = if progress.number {
                    digits.at(x, y)
                } else {
                    viewfinder(size, x, y)
                };
                if on { 1.0 } else { 0.0 }
            } else if y < top || x < left || x >= right {
                0.0
            } else {
                let edge_x = x < left + line || x >= right - line;
                let edge_y = y < top + line || y >= size - line;
                match (edge_x, edge_y) {
                    // Rounded off: the outline's corner pixels are left out.
                    (true, true) => 0.0,
                    (true, false) | (false, true) => 1.0,
                    // Partly filled at the edge, so it moves smoothly
                    // rather than a whole pixel at a time.
                    (false, false) => (fill - x as f32).clamp(0.0, 1.0),
                }
            };
            let [r, g, b, a] = color;
            rgba.extend_from_slice(&[r, g, b, (f32::from(a) * alpha).round() as u8]);
        }
    }
    rgba
}

/// Builds an icon of `color` where `on`.
fn pixels(size: u32, color: [u8; 4], on: impl Fn(u32, u32) -> bool) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            rgba.extend_from_slice(if on(x, y) { &color } else { &CLEAR });
        }
    }
    rgba
}

fn viewfinder(size: u32, x: u32, y: u32) -> bool {
    let s = size as f32;
    let margin = s * 0.08;
    let thick = (s * 0.11).max(1.5);
    let arm = s * 0.34;
    let dot = s * 0.12;
    let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
    // Distance from the nearest edge, mirrored so all four corners match.
    let dx = fx.min(s - fx) - margin;
    let dy = fy.min(s - fy) - margin;
    let corner = dx >= 0.0 && dy >= 0.0 && ((dx < thick && dy < arm) || (dy < thick && dx < arm));
    let center = (fx - s / 2.0).abs() < dot && (fy - s / 2.0).abs() < dot;
    corner || center
}

/// A percentage drawn across the top of the icon.
struct Digits {
    digits: Vec<u16>,
    /// Icon pixels per font pixel.
    scale: u32,
    x0: u32,
    y0: u32,
    height: u32,
}

impl Digits {
    fn new(size: u32, percent: u8) -> Self {
        // Two digits at most: 100 doesn't fit, and only shows while the
        // last response comes back.
        let digits: Vec<u16> = percent
            .min(99)
            .to_string()
            .bytes()
            .map(|d| DIGITS[usize::from(d - b'0')])
            .collect();
        let scale = (size / 8).max(1);
        // A font pixel between digits.
        let width = (4 * digits.len() as u32 - 1) * scale;
        Self {
            digits,
            scale,
            x0: size.saturating_sub(width) / 2,
            y0: 0,
            height: 5 * scale,
        }
    }

    fn at(&self, x: u32, y: u32) -> bool {
        let (Some(dx), Some(dy)) = (x.checked_sub(self.x0), y.checked_sub(self.y0)) else {
            return false;
        };
        let (dx, dy) = (dx / self.scale, dy / self.scale);
        if dy >= 5 || dx % 4 == 3 {
            return false;
        }
        self.digits
            .get((dx / 4) as usize)
            .is_some_and(|bits| bits >> ((4 - dy) * 3 + (2 - dx % 4)) & 1 == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Which pixels of a 16px icon are drawn, a row per string.
    fn rows(rgba: &[u8]) -> Vec<String> {
        rgba.chunks_exact(16 * 4)
            .map(|row| row.chunks_exact(4).map(|p| if p[3] > 0 { '#' } else { '.' }).collect())
            .collect()
    }

    fn icon(percent: u8, number: bool) -> Vec<String> {
        rows(&icon_with_progress(16, Some(Progress { percent, number }), BLUE))
    }

    #[test]
    fn draws_the_upload_progress_as_a_bar_across_the_bottom() {
        assert_eq!(icon_rgba(16), icon_with_progress(16, None, BLUE));
        let half = icon(50, false);
        assert_eq!(half[9], "................");
        assert_eq!(half[10], "..############..");
        assert_eq!(half[12], ".#######......#.");
        assert_eq!(half[15], "..############..");
        assert_eq!(icon(0, false)[12], ".#............#.");
        assert_eq!(icon(100, false)[12], ".##############.");
        // Between whole pixels, the last one is partly filled.
        let rgba = icon_with_progress(16, Some(Progress { percent: 54, number: false }), BLUE);
        let alpha = |x: usize| rgba[(12 * 16 + x) * 4 + 3];
        assert_eq!((alpha(7), alpha(8), alpha(9)), (255, 122, 0));
    }

    #[test]
    fn draws_in_the_colour_asked_for() {
        let progress = Some(Progress { percent: 30, number: true });
        for style in [TrayIconStyle::Blue, TrayIconStyle::White, TrayIconStyle::Black] {
            let c = color(style);
            for p in [None, progress] {
                let rgba = icon_with_progress(16, p, c);
                // Every drawn pixel is that colour, and only its alpha varies.
                assert!(rgba.chunks_exact(4).all(|px| px[3] == 0 || px[..3] == c[..3]));
                // The same shape whatever the colour.
                assert_eq!(rows(&rgba), rows(&icon_with_progress(16, p, BLUE)));
            }
        }
    }

    #[test]
    fn can_show_the_percentage_above_the_bar() {
        let n = icon(47, true);
        // 4 and 7, each font pixel 2x2.
        assert_eq!(n[0], ".##..##..######.");
        assert_eq!(n[4], ".######......##.");
        assert_eq!(n[9], ".....##......##.");
        assert_eq!(n[10], "................");
        assert_eq!(n[11], "..############..");
        assert_eq!(n[13], ".#######......#.");
        assert_eq!(n[15], "..############..");
        // One digit is centred.
        assert_eq!(icon(5, true)[0], ".....######.....");
        assert_eq!(icon(100, true)[0], icon(99, true)[0]);
    }

    /// Writes the app icon at the sizes macOS asks of an app to
    /// `assets/snapr.iconset`, which scripts/macos-bundle.sh makes the
    /// app's icon from. Rerun after changing the icon:
    /// `cargo test write_iconset -- --ignored`.
    #[test]
    #[ignore]
    fn write_iconset() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/snapr.iconset");
        std::fs::create_dir_all(&dir).unwrap();
        for size in [16, 32, 128, 256, 512] {
            for (scale, suffix) in [(1, ""), (2, "@2x")] {
                let px = size * scale;
                image::RgbaImage::from_raw(px, px, icon_rgba(px))
                    .unwrap()
                    .save(dir.join(format!("icon_{size}x{size}{suffix}.png")))
                    .unwrap();
            }
        }
    }
}
