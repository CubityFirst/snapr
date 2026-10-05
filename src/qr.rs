//! Reading QR codes out of a captured region, and making new ones.

use image::{Rgba, RgbaImage};
use qrcode::{Color, QrCode};

/// Pixels per module of a generated code.
const MODULE: u32 = 10;
/// White border around a generated code, in modules (the spec asks for 4).
const QUIET_ZONE: u32 = 4;
/// Largest side a region is upscaled to when looking for small codes.
const MAX_UPSCALED: u32 = 2400;

/// The text of every QR code found in `img`. Non-UTF-8 contents are decoded
/// lossily.
pub fn decode(img: &RgbaImage) -> Vec<String> {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let grey: Vec<u8> = img
        .pixels()
        .map(|p| ((p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114) / 1000) as u8)
        .collect();
    // On screen, codes are often tiny (a pixel or two per module) or light on
    // dark; try as is first, then inverted, then enlarged.
    let mut passes = vec![(1, false), (1, true)];
    for scale in [2, 3] {
        if w.max(h) * scale <= MAX_UPSCALED {
            passes.extend([(scale, false), (scale, true)]);
        }
    }
    for (scale, invert) in passes {
        let found = scan(&grey, w, h, scale, invert);
        if !found.is_empty() {
            return found;
        }
    }
    Vec::new()
}

/// One decoding pass over the greyscale image, enlarged `scale` times and
/// framed with a white margin, so a tightly cropped code still has the
/// quiet zone the detector needs.
fn scan(grey: &[u8], w: u32, h: u32, scale: u32, invert: bool) -> Vec<String> {
    let pad = (w.max(h) * scale / 10).max(8);
    let (pw, ph) = (w * scale + 2 * pad, h * scale + 2 * pad);
    let mut prepared =
        rqrr::PreparedImage::prepare_from_greyscale(pw as usize, ph as usize, |x, y| {
            let (x, y) = (x as u32, y as u32);
            if x < pad || y < pad || x >= pad + w * scale || y >= pad + h * scale {
                return 255;
            }
            let v = grey[((y - pad) / scale * w + (x - pad) / scale) as usize];
            if invert { 255 - v } else { v }
        });
    let mut found = Vec::new();
    for grid in prepared.detect_grids() {
        let mut bytes = Vec::new();
        if grid.decode_to(&mut bytes).is_ok() {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            if !found.contains(&text) {
                found.push(text);
            }
        }
    }
    found
}

/// A QR code for `text`, black on white with a quiet zone.
pub fn encode(text: &str) -> Result<RgbaImage, String> {
    let code = QrCode::new(text.as_bytes()).map_err(|e| e.to_string())?;
    let modules = code.width() as u32;
    let colors = code.to_colors();
    let side = (modules + 2 * QUIET_ZONE) * MODULE;
    Ok(RgbaImage::from_fn(side, side, |x, y| {
        let (mx, my) = (x / MODULE, y / MODULE);
        let inside = (QUIET_ZONE..QUIET_ZONE + modules).contains(&mx)
            && (QUIET_ZONE..QUIET_ZONE + modules).contains(&my);
        let dark = inside
            && colors[((my - QUIET_ZONE) * modules + mx - QUIET_ZONE) as usize] == Color::Dark;
        if dark {
            Rgba([0, 0, 0, 255])
        } else {
            Rgba([255, 255, 255, 255])
        }
    }))
}

/// Whether decoded text is a link worth offering to open.
pub fn is_link(text: &str) -> bool {
    let t = text.trim();
    !t.contains(char::is_whitespace)
        && ["http://", "https://"]
            .iter()
            .any(|p| t.len() > p.len() && t[..p.len()].eq_ignore_ascii_case(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let img = encode("https://example.com/snapr?q=1").unwrap();
        assert_eq!(decode(&img), vec!["https://example.com/snapr?q=1"]);
    }

    #[test]
    fn reads_a_tightly_cropped_code() {
        let img = encode("no margin").unwrap();
        let q = QUIET_ZONE * MODULE;
        let side = img.width() - 2 * q;
        let cropped = image::imageops::crop_imm(&img, q, q, side, side).to_image();
        assert_eq!(decode(&cropped), vec!["no margin"]);
    }

    #[test]
    fn reads_a_tiny_code() {
        // One pixel per module, as a small code on screen might be.
        let img = encode("tiny").unwrap();
        let small = image::imageops::resize(
            &img,
            img.width() / MODULE,
            img.height() / MODULE,
            image::imageops::FilterType::Nearest,
        );
        assert_eq!(decode(&small), vec!["tiny"]);
    }

    #[test]
    fn reads_light_on_dark() {
        let mut img = encode("inverted").unwrap();
        image::imageops::invert(&mut img);
        assert_eq!(decode(&img), vec!["inverted"]);
    }

    #[test]
    fn finds_nothing_in_a_blank_region() {
        assert!(decode(&RgbaImage::from_pixel(200, 120, Rgba([40, 40, 40, 255]))).is_empty());
    }

    #[test]
    fn links() {
        assert!(is_link("https://example.com"));
        assert!(is_link("HTTP://EXAMPLE.COM/x"));
        assert!(!is_link("https://"));
        assert!(!is_link("hello https://example.com"));
        assert!(!is_link("WIFI:S:home;T:WPA;P:secret;;"));
    }
}
