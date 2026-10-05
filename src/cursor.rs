//! The mouse pointer, for drawing into recordings: DXGI desktop duplication
//! leaves it out of the frames.

/// A cursor image, premultiplied RGBA, with its hotspot.
#[derive(Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub hotspot: (i32, i32),
    /// Premultiplied RGBA.
    pub pixels: Vec<u8>,
}

/// Where the pointer is (global physical pixels) and what it looks like,
/// if it's showing.
pub struct Cursors {
    #[cfg(windows)]
    cache: Option<(usize, Image)>,
}

impl Cursors {
    pub fn new() -> Self {
        Self {
            #[cfg(windows)]
            cache: None,
        }
    }

    /// The current pointer: its top-left corner and image.
    #[cfg(windows)]
    pub fn current(&mut self) -> Option<((i32, i32), &Image)> {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CURSOR_SHOWING, CURSORINFO, GetCursorInfo,
        };
        let mut info: CURSORINFO = unsafe { std::mem::zeroed() };
        info.cbSize = size_of::<CURSORINFO>() as u32;
        // SAFETY: `info` is a properly sized CURSORINFO.
        if unsafe { GetCursorInfo(&mut info) } == 0
            || info.flags & CURSOR_SHOWING == 0
            || info.hCursor.is_null()
        {
            return None;
        }
        let handle = info.hCursor as usize;
        if self.cache.as_ref().is_none_or(|(h, _)| *h != handle) {
            self.cache = Some((handle, render(info.hCursor)?));
        }
        let image = &self.cache.as_ref()?.1;
        Some((
            (
                info.ptScreenPos.x - image.hotspot.0,
                info.ptScreenPos.y - image.hotspot.1,
            ),
            image,
        ))
    }

    #[cfg(not(windows))]
    pub fn current(&mut self) -> Option<((i32, i32), &Image)> {
        None
    }
}

/// Draws the cursor onto black and onto white; the difference gives each
/// pixel's coverage, which handles colour, alpha and monochrome cursors.
#[cfg(windows)]
fn render(cursor: windows_sys::Win32::UI::WindowsAndMessaging::HCURSOR) -> Option<Image> {
    use windows_sys::Win32::Graphics::Gdi::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DI_NORMAL, DrawIconEx, GetIconInfo, ICONINFO,
    };

    // SAFETY: plain GDI calls; every object created here is released below.
    unsafe {
        let mut info: ICONINFO = std::mem::zeroed();
        if GetIconInfo(cursor, &mut info) == 0 {
            return None;
        }
        // Size from the colour bitmap, or the mask (twice as tall: AND + XOR)
        // for monochrome cursors.
        let mut bm: BITMAP = std::mem::zeroed();
        let source = if info.hbmColor.is_null() {
            info.hbmMask
        } else {
            info.hbmColor
        };
        GetObjectW(
            source,
            size_of::<BITMAP>() as i32,
            &mut bm as *mut _ as *mut _,
        );
        let width = bm.bmWidth.max(1) as u32;
        let height = if info.hbmColor.is_null() {
            bm.bmHeight / 2
        } else {
            bm.bmHeight
        }
        .max(1) as u32;
        let hotspot = (info.xHotspot as i32, info.yHotspot as i32);
        if !info.hbmColor.is_null() {
            DeleteObject(info.hbmColor);
        }
        if !info.hbmMask.is_null() {
            DeleteObject(info.hbmMask);
        }

        let draw_on = |background: u8| -> Option<Vec<u8>> {
            let dc = CreateCompatibleDC(std::ptr::null_mut());
            let mut bmi: BITMAPINFO = std::mem::zeroed();
            bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = width as i32;
            bmi.bmiHeader.biHeight = -(height as i32); // top-down
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB;
            let mut bits = std::ptr::null_mut();
            let bitmap =
                CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
            if bitmap.is_null() || bits.is_null() {
                DeleteDC(dc);
                return None;
            }
            let old = SelectObject(dc, bitmap);
            let len = (width * height * 4) as usize;
            std::ptr::write_bytes(bits as *mut u8, background, len);
            DrawIconEx(
                dc,
                0,
                0,
                cursor,
                width as i32,
                height as i32,
                0,
                std::ptr::null_mut(),
                DI_NORMAL,
            );
            let pixels = std::slice::from_raw_parts(bits as *const u8, len).to_vec();
            SelectObject(dc, old);
            DeleteObject(bitmap);
            DeleteDC(dc);
            Some(pixels)
        };
        let black = draw_on(0)?;
        let white = draw_on(255)?;

        // BGRA from GDI. On black a pixel is c·a, on white c·a + (1 − a), so
        // a = 1 − (white − black) and the black drawing is already
        // premultiplied.
        let mut pixels = vec![0u8; black.len()];
        for i in (0..black.len()).step_by(4) {
            let diff = (0..3)
                .map(|c| white[i + c].saturating_sub(black[i + c]) as u32)
                .max()
                .unwrap_or(255);
            let alpha = 255 - diff.min(255) as u8;
            pixels[i] = black[i + 2].min(alpha);
            pixels[i + 1] = black[i + 1].min(alpha);
            pixels[i + 2] = black[i].min(alpha);
            pixels[i + 3] = alpha;
        }
        Some(Image {
            width,
            height,
            hotspot,
            pixels,
        })
    }
}

/// Blends `cursor` (premultiplied) into an RGBA frame `frame_w` wide, with
/// its top-left corner at `at` in frame pixels.
pub fn blend(frame: &mut [u8], frame_w: u32, frame_h: u32, cursor: &Image, at: (i32, i32)) {
    for cy in 0..cursor.height as i32 {
        let y = at.1 + cy;
        if y < 0 || y >= frame_h as i32 {
            continue;
        }
        for cx in 0..cursor.width as i32 {
            let x = at.0 + cx;
            if x < 0 || x >= frame_w as i32 {
                continue;
            }
            let s = ((cy as u32 * cursor.width + cx as u32) * 4) as usize;
            let a = cursor.pixels[s + 3] as u32;
            if a == 0 {
                continue;
            }
            let d = ((y as u32 * frame_w + x as u32) * 4) as usize;
            for c in 0..3 {
                let under = frame[d + c] as u32;
                frame[d + c] =
                    (cursor.pixels[s + c] as u32 + under * (255 - a) / 255).min(255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blends_premultiplied_pixels_and_clips() {
        // 2x1 cursor: opaque red, then half-transparent white.
        let cursor = Image {
            width: 2,
            height: 1,
            hotspot: (0, 0),
            pixels: vec![255, 0, 0, 255, 128, 128, 128, 128],
        };
        let mut frame = vec![0u8; 3 * 4]; // 3x1 black
        blend(&mut frame, 3, 1, &cursor, (1, 0));
        assert_eq!(&frame[4..8], &[255, 0, 0, 0]);
        assert_eq!(&frame[8..11], &[128, 128, 128]);
        // Mostly off the frame: nothing panics, the visible pixel is drawn.
        let mut frame = vec![0u8; 4];
        blend(&mut frame, 1, 1, &cursor, (-1, 0));
        assert_eq!(&frame[0..3], &[128, 128, 128]);
    }

    /// The real pointer renders to something visible:
    /// `cargo test current_cursor -- --ignored`.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn current_cursor_renders() {
        let mut cursors = Cursors::new();
        let (_, image) = cursors.current().expect("cursor showing");
        assert!(image.width >= 16 && image.height >= 16);
        assert!(
            image.pixels.chunks(4).any(|p| p[3] == 255),
            "no opaque pixels"
        );
        assert!(
            image.pixels.chunks(4).any(|p| p[3] == 0),
            "no transparent pixels"
        );
    }
}
