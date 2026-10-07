use image::RgbaImage;

/// An integer rectangle in global (virtual desktop) physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn from_points(a: (f64, f64), b: (f64, f64)) -> Self {
        let (x0, x1) = (a.0.min(b.0).floor() as i32, a.0.max(b.0).ceil() as i32);
        let (y0, y1) = (a.1.min(b.1).floor() as i32, a.1.max(b.1).ceil() as i32);
        Self {
            x: x0,
            y: y0,
            w: (x1 - x0) as u32,
            h: (y1 - y0) as u32,
        }
    }

    pub fn right(&self) -> i32 {
        self.x + self.w as i32
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h as i32
    }

    pub fn contains(&self, (x, y): (f64, f64)) -> bool {
        x >= self.x as f64
            && y >= self.y as f64
            && x < self.right() as f64
            && y < self.bottom() as f64
    }

    /// The smallest rectangle containing both.
    pub fn union(&self, other: &Rect) -> Rect {
        let (x0, y0) = (self.x.min(other.x), self.y.min(other.y));
        let (x1, y1) = (
            self.right().max(other.right()),
            self.bottom().max(other.bottom()),
        );
        Rect {
            x: x0,
            y: y0,
            w: (x1 - x0) as u32,
            h: (y1 - y0) as u32,
        }
    }

    pub fn intersect(&self, other: &Rect) -> Option<Rect> {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = self.right().min(other.right());
        let y1 = self.bottom().min(other.bottom());
        (x1 > x0 && y1 > y0).then(|| Rect {
            x: x0,
            y: y0,
            w: (x1 - x0) as u32,
            h: (y1 - y0) as u32,
        })
    }
}

/// A frozen frame of one monitor.
pub struct Shot {
    pub image: RgbaImage,
    /// Best guess of the monitor's top-left corner in physical pixels, used to
    /// match it to the windowing system's monitor list.
    pub pos: (i32, i32),
}

/// A shot together with the global rectangle its overlay window covers.
pub struct PlacedShot<'a> {
    pub image: &'a RgbaImage,
    pub rect: Rect,
}

pub fn capture_all() -> Result<Vec<Shot>, xcap::XCapError> {
    // Displays in HDR mode are captured in full range and mapped to SDR.
    #[cfg(windows)]
    let hdr = crate::hdr::displays();
    xcap::Monitor::all()?
        .iter()
        .map(|m| {
            let (x, y) = (m.x()?, m.y()?);
            #[cfg(windows)]
            let image = match hdr.iter().find(|d| (d.rect.x, d.rect.y) == (x, y)) {
                Some(d) => d.capture().or_else(|e| {
                    eprintln!("{e}; capturing it as SDR");
                    m.capture_image()
                })?,
                None => m.capture_image()?,
            };
            #[cfg(not(windows))]
            let image = m.capture_image()?;
            // macOS reports monitor positions in points rather than pixels.
            let scale = if cfg!(target_os = "macos") {
                m.scale_factor()?
            } else {
                1.0
            };
            let pos = (
                (x as f32 * scale).round() as i32,
                (y as f32 * scale).round() as i32,
            );
            Ok(Shot { image, pos })
        })
        .collect()
}

/// The mouse position in global physical pixels, where the platform makes
/// it cheap to ask (Windows). Elsewhere it's learned from the first move.
pub fn cursor_position() -> Option<(f64, f64)> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::POINT;
        use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
        let mut p = POINT { x: 0, y: 0 };
        // SAFETY: writes into a valid POINT.
        if unsafe { GetCursorPos(&mut p) } != 0 {
            return Some((p.x as f64, p.y as f64));
        }
    }
    None
}

/// The visible windows' frames (global physical pixels), topmost first, for
/// snapping the region to a window. Must be called before our overlays open.
/// `own` are snapr's windows to include (the main window, pins): on Windows
/// xcap leaves out the calling process's windows.
#[cfg_attr(not(windows), allow(unused_variables))]
pub fn window_rects(own: &[&winit::window::Window]) -> Vec<Rect> {
    let Ok(windows) = xcap::Window::all() else {
        return Vec::new();
    };
    let others = windows
        .into_iter()
        .filter(|w| !w.is_minimized().unwrap_or(true))
        // Untitled windows are mostly invisible helpers and overlays.
        .filter(|w| w.title().is_ok_and(|t| !t.trim().is_empty()))
        .filter_map(|w| {
            let rect = Rect {
                x: w.x().ok()?,
                y: w.y().ok()?,
                w: w.width().ok()?,
                h: w.height().ok()?,
            };
            Some((w.id().ok()?, rect))
        });
    #[cfg(windows)]
    let others = win32_windows::with_own(others.collect(), own);
    others
        .map(|(_, r)| r)
        .filter(|r| r.w >= 8 && r.h >= 8)
        .collect()
}

#[cfg(windows)]
mod win32_windows {
    use std::collections::HashMap;

    use windows_sys::Win32::Foundation::{HWND, RECT};
    use windows_sys::Win32::Graphics::Dwm::{
        DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GW_HWNDNEXT, GetTopWindow, GetWindow, IsIconic, IsWindowVisible,
    };
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::Rect;

    /// `others` (xcap's windows, by id) and `own`, in z-order, topmost first.
    pub fn with_own(
        others: Vec<(u32, Rect)>,
        own: &[&winit::window::Window],
    ) -> impl Iterator<Item = (u32, Rect)> {
        let mut by_id: HashMap<u32, Rect> = others.iter().copied().collect();
        by_id.extend(own.iter().filter_map(|w| {
            let RawWindowHandle::Win32(h) = w.window_handle().ok()?.as_raw() else {
                return None;
            };
            let hwnd = h.hwnd.get() as HWND;
            Some((hwnd as usize as u32, frame(hwnd)?))
        }));
        let mut ordered = Vec::with_capacity(by_id.len());
        // SAFETY: walks the top-level windows; a null handle ends it.
        let mut hwnd = unsafe { GetTopWindow(std::ptr::null_mut()) };
        while !hwnd.is_null() {
            // xcap's ids are its handles, cut to 32 bits.
            if let Some(r) = by_id.remove(&(hwnd as usize as u32)) {
                ordered.push((hwnd as usize as u32, r));
            }
            // SAFETY: as above.
            hwnd = unsafe { GetWindow(hwnd, GW_HWNDNEXT) };
        }
        ordered.into_iter()
    }

    /// The window's frame as xcap measures others' (without the invisible
    /// resize borders), if it's showing.
    fn frame(hwnd: HWND) -> Option<Rect> {
        let mut cloaked = 0u32;
        let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        // SAFETY: `hwnd` is a live window of ours; the out-pointers are valid
        // and sized as given.
        let showing = unsafe {
            IsWindowVisible(hwnd) != 0
                && IsIconic(hwnd) == 0
                && (DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED as u32, (&raw mut cloaked).cast(), 4) != 0
                    || cloaked == 0)
                && DwmGetWindowAttribute(
                    hwnd,
                    DWMWA_EXTENDED_FRAME_BOUNDS as u32,
                    (&raw mut r).cast(),
                    size_of::<RECT>() as u32,
                ) == 0
        };
        (showing && r.right > r.left && r.bottom > r.top).then(|| Rect {
            x: r.left,
            y: r.top,
            w: (r.right - r.left) as u32,
            h: (r.bottom - r.top) as u32,
        })
    }
}

/// Process name (e.g. `chrome`) and title of the focused window. Must be
/// called before our overlay windows take focus.
pub fn foreground_window() -> (Option<String>, Option<String>) {
    let Some(window) = xcap::Window::all()
        .ok()
        .and_then(|all| all.into_iter().find(|w| w.is_focused().unwrap_or(false)))
    else {
        return (None, None);
    };
    let by_pid = window.pid().ok().and_then(process_name);
    let process = by_pid
        .or_else(|| window.app_name().ok())
        .filter(|n| !n.is_empty());
    let title = window.title().ok().filter(|t| !t.is_empty());
    (process, title)
}

#[cfg(windows)]
fn process_name(pid: u32) -> Option<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: the handle is checked and closed, and `buf`/`len` describe a valid buffer.
    let ok = unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        ok != 0
    };
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    ok.then(|| {
        std::path::Path::new(&path)
            .file_stem()?
            .to_str()
            .map(str::to_owned)
    })
    .flatten()
}

#[cfg(target_os = "linux")]
fn process_name(pid: u32) -> Option<String> {
    Some(
        std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .ok()?
            .trim()
            .to_owned(),
    )
}

/// macOS: xcap's app name is already the owning process's name.
#[cfg(not(any(windows, target_os = "linux")))]
fn process_name(_pid: u32) -> Option<String> {
    None
}

/// What to capture without picking a region on the frozen screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The same region as the last screenshot.
    Region(Rect),
    /// One window, by its xcap id.
    Window(u32),
    /// One monitor, by its xcap id.
    Display(u32),
    /// Every monitor, stitched together.
    Everything,
}

/// An app's window, for the capture menu.
#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub id: u32,
    pub app: String,
    pub title: String,
}

/// A monitor, for the capture menu.
#[derive(Debug, Clone)]
pub struct DisplayInfo {
    pub id: u32,
    pub name: String,
    pub rect: Rect,
    pub primary: bool,
}

/// Other apps' visible windows, topmost first.
pub fn windows() -> Vec<WindowInfo> {
    let Ok(windows) = xcap::Window::all() else {
        return Vec::new();
    };
    let me = std::process::id();
    windows
        .into_iter()
        .filter(|w| !w.is_minimized().unwrap_or(true))
        .filter(|w| w.pid().is_ok_and(|p| p != me))
        .filter(|w| w.width().unwrap_or(0) >= 8 && w.height().unwrap_or(0) >= 8)
        .filter_map(|w| {
            let title = w.title().ok().filter(|t| !t.trim().is_empty())?;
            let app = w
                .pid()
                .ok()
                .and_then(process_name)
                .or_else(|| w.app_name().ok())
                .unwrap_or_default();
            Some(WindowInfo {
                id: w.id().ok()?,
                app,
                title,
            })
        })
        .collect()
}

/// The monitors, left to right.
pub fn displays() -> Vec<DisplayInfo> {
    let Ok(monitors) = xcap::Monitor::all() else {
        return Vec::new();
    };
    let mut list: Vec<_> = monitors
        .iter()
        .filter_map(|m| {
            let name = m
                .friendly_name()
                .ok()
                .filter(|n| !n.trim().is_empty())
                .or_else(|| m.name().ok())
                .unwrap_or_default();
            Some(DisplayInfo {
                id: m.id().ok()?,
                name,
                rect: Rect {
                    x: m.x().ok()?,
                    y: m.y().ok()?,
                    w: m.width().ok()?,
                    h: m.height().ok()?,
                },
                primary: m.is_primary().unwrap_or(false),
            })
        })
        .collect();
    list.sort_by_key(|d| (d.rect.x, d.rect.y));
    list
}

/// Captures `target` straight away, with the naming details of what was
/// captured (process and title).
pub fn grab(target: &Target) -> Result<(RgbaImage, Option<String>, Option<String>), String> {
    let failed = |e: xcap::XCapError| format!("screen capture failed: {e}");
    match target {
        Target::Window(id) => {
            let window = xcap::Window::all()
                .map_err(failed)?
                .into_iter()
                .find(|w| w.id().is_ok_and(|i| i == *id))
                .ok_or("that window has closed")?;
            let image = window.capture_image().map_err(failed)?;
            let process = window
                .pid()
                .ok()
                .and_then(process_name)
                .or_else(|| window.app_name().ok())
                .filter(|n| !n.is_empty());
            let title = window.title().ok().filter(|t| !t.is_empty());
            Ok((image, process, title))
        }
        Target::Display(id) => {
            let monitor = xcap::Monitor::all()
                .map_err(failed)?
                .into_iter()
                .find(|m| m.id().is_ok_and(|i| i == *id))
                .ok_or("that display is no longer connected")?;
            let image = monitor.capture_image().map_err(failed)?;
            let (process, title) = foreground_window();
            Ok((image, process, title))
        }
        Target::Region(_) | Target::Everything => {
            let (process, title) = foreground_window();
            let shots = capture_all().map_err(failed)?;
            let placed: Vec<_> = shots
                .iter()
                .map(|s| PlacedShot {
                    image: &s.image,
                    rect: Rect {
                        x: s.pos.0,
                        y: s.pos.1,
                        w: s.image.width(),
                        h: s.image.height(),
                    },
                })
                .collect();
            let region = match target {
                Target::Region(r) => *r,
                _ => placed
                    .iter()
                    .map(|s| s.rect)
                    .reduce(|a, b| a.union(&b))
                    .ok_or("no monitors found")?,
            };
            let image = crop(&placed, region).ok_or("that region is off the screen")?;
            Ok((image, process, title))
        }
    }
}

/// Builds the final screenshot for `region`, stitching together every monitor
/// it overlaps. Areas not covered by any monitor are left transparent.
pub fn crop(shots: &[PlacedShot], region: Rect) -> Option<RgbaImage> {
    if region.w == 0 || region.h == 0 {
        return None;
    }
    let mut out = RgbaImage::new(region.w, region.h);
    let mut any = false;
    for shot in shots {
        let Some(inter) = region.intersect(&shot.rect) else {
            continue;
        };
        any = true;
        let (iw, ih) = shot.image.dimensions();
        // Overlay windows normally match the capture 1:1, but scale in case
        // the platform gave us a differently sized window.
        let sx = iw as f64 / shot.rect.w as f64;
        let sy = ih as f64 / shot.rect.h as f64;
        for gy in inter.y..inter.bottom() {
            let src_y = (((gy - shot.rect.y) as f64 * sy) as u32).min(ih - 1);
            for gx in inter.x..inter.right() {
                let src_x = (((gx - shot.rect.x) as f64 * sx) as u32).min(iw - 1);
                let px = *shot.image.get_pixel(src_x, src_y);
                out.put_pixel((gx - region.x) as u32, (gy - region.y) as u32, px);
            }
        }
    }
    any.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn crop_stitches_across_monitors() {
        let left = RgbaImage::from_pixel(10, 10, Rgba([255, 0, 0, 255]));
        let right = RgbaImage::from_pixel(10, 10, Rgba([0, 0, 255, 255]));
        let shots = [
            PlacedShot {
                image: &left,
                rect: Rect {
                    x: 0,
                    y: 0,
                    w: 10,
                    h: 10,
                },
            },
            PlacedShot {
                image: &right,
                rect: Rect {
                    x: 10,
                    y: 0,
                    w: 10,
                    h: 10,
                },
            },
        ];
        let out = crop(&shots, Rect::from_points((8.0, 2.0), (12.0, 4.0))).unwrap();
        assert_eq!(out.dimensions(), (4, 2));
        assert_eq!(out.get_pixel(1, 0), &Rgba([255, 0, 0, 255]));
        assert_eq!(out.get_pixel(2, 1), &Rgba([0, 0, 255, 255]));
    }

    #[test]
    fn crop_outside_monitors_is_none() {
        let img = RgbaImage::new(10, 10);
        let shots = [PlacedShot {
            image: &img,
            rect: Rect {
                x: 0,
                y: 0,
                w: 10,
                h: 10,
            },
        }];
        assert!(
            crop(
                &shots,
                Rect {
                    x: 50,
                    y: 50,
                    w: 5,
                    h: 5
                }
            )
            .is_none()
        );
    }
}
