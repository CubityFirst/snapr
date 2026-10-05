//! Previews for any file snapr saved or uploaded: images are decoded, videos
//! show a poster frame (cached; decoded from videos snapr didn't record),
//! and everything else gets a generic file icon.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use egui::{Color32, FontId, Pos2, Rect, Stroke, pos2, vec2};
use image::{DynamicImage, RgbaImage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Image,
    Video,
    File,
}

pub fn kind(path: &Path) -> Kind {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" => Kind::Image,
        "mp4" | "mov" | "webm" | "mkv" | "avi" | "m4v" => Kind::Video,
        _ => Kind::File,
    }
}

/// The picture to preview `path` with (full size, not yet shrunk), if it has
/// one: the image itself, or a video's poster frame.
pub fn picture(path: &Path, ffmpeg: &str) -> Option<DynamicImage> {
    match kind(path) {
        Kind::Image => image::open(path).ok(),
        Kind::Video => {
            let poster = poster_path(path)?;
            if !poster.is_file() {
                extract_frame(path, &poster, ffmpeg)?;
            }
            image::open(poster).ok()
        }
        Kind::File => None,
    }
}

/// Remembers a recording's first frame as its poster.
pub fn save_poster(video: &Path, frame: &RgbaImage) {
    let Some(poster) = poster_path(video) else {
        return;
    };
    if let Some(dir) = poster.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Small enough for previews; full-size frames are big PNGs.
    let small = DynamicImage::ImageRgba8(frame.clone()).thumbnail(1920, 1200);
    if let Err(e) = small.save(&poster) {
        eprintln!("couldn't save the video's preview: {e}");
    }
}

/// Where a video's poster frame is cached. The name includes the file's size
/// and time, so a replaced file gets a new poster.
fn poster_path(video: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(video).ok()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    video.hash(&mut h);
    meta.len().hash(&mut h);
    meta.modified().ok().hash(&mut h);
    Some(
        dirs::cache_dir()?
            .join("snapr")
            .join("posters")
            .join(format!("{:016x}.png", h.finish())),
    )
}

/// Saves the first frame of a video, at most 1920 pixels wide, as `out`.
/// `ffmpeg` decodes it where the OS can't (Linux).
fn extract_frame(video: &Path, out: &Path, ffmpeg: &str) -> Option<()> {
    std::fs::create_dir_all(out.parent()?).ok()?;
    let frame = crate::decode::first_picture(video, ffmpeg, (1920, u32::MAX))?;
    frame.save(out).ok()
}

/// Shrinks a picture to fit `max` and converts it for egui, with its
/// original size.
pub fn to_egui(img: DynamicImage, max: (u32, u32)) -> (egui::ColorImage, [u32; 2]) {
    let full = [img.width(), img.height()];
    let img = if img.width() > max.0 || img.height() > max.1 {
        img.thumbnail(max.0, max.1)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    (
        egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()),
        full,
    )
}

/// A generic document (folded corner, extension underneath) centred in
/// `area`, sized to fit it.
pub fn draw_file_icon(painter: &egui::Painter, area: Rect, path: &Path) {
    let h = (area.height() * 0.6).min(area.width() * 0.5).min(96.0);
    let w = h * 0.78;
    let page = Rect::from_center_size(area.center() - vec2(0.0, h * 0.08), vec2(w, h));
    let fold = w * 0.3;
    let fill = Color32::from_rgb(58, 61, 68);
    let edge = Stroke::new(1.5, Color32::from_rgb(120, 124, 134));
    let outline = vec![
        page.left_top(),
        pos2(page.right() - fold, page.top()),
        pos2(page.right(), page.top() + fold),
        page.right_bottom(),
        page.left_bottom(),
    ];
    painter.add(egui::Shape::convex_polygon(outline, fill, edge));
    let corner = pos2(page.right() - fold, page.top() + fold);
    painter.add(egui::Shape::convex_polygon(
        vec![
            pos2(page.right() - fold, page.top()),
            pos2(page.right(), page.top() + fold),
            corner,
        ],
        Color32::from_rgb(88, 92, 102),
        edge,
    ));
    // A few lines of "text" on the page.
    let line = Stroke::new(1.5, Color32::from_rgb(100, 104, 114));
    for i in 0..3 {
        let y = page.top() + h * (0.45 + 0.13 * i as f32);
        let right = if i == 2 {
            page.center().x
        } else {
            page.right() - w * 0.18
        };
        painter.line_segment([pos2(page.left() + w * 0.18, y), pos2(right, y)], line);
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_uppercase())
        .unwrap_or_else(|| "FILE".into());
    painter.text(
        pos2(page.center().x, page.bottom() + 6.0),
        egui::Align2::CENTER_TOP,
        ext,
        FontId::proportional((h * 0.16).clamp(10.0, 14.0)),
        Color32::from_rgb(170, 174, 184),
    );
}

/// A round play button over a video's preview.
pub fn draw_play_badge(painter: &egui::Painter, center: Pos2, radius: f32) {
    painter.circle_filled(center, radius, Color32::from_black_alpha(150));
    let r = radius * 0.5;
    painter.add(egui::Shape::convex_polygon(
        vec![
            center + vec2(-r * 0.6, -r),
            center + vec2(r * 1.1, 0.0),
            center + vec2(-r * 0.6, r),
        ],
        Color32::WHITE,
        Stroke::NONE,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_files_by_kind() {
        assert_eq!(kind(Path::new("a/b.PNG")), Kind::Image);
        assert_eq!(kind(Path::new("clip.mp4")), Kind::Video);
        assert_eq!(kind(Path::new("notes.txt")), Kind::File);
        assert_eq!(kind(Path::new("noext")), Kind::File);
    }

    /// Renders a file icon to `target/file-icon-preview.png`:
    /// `cargo test file_icon_preview -- --ignored`.
    #[test]
    #[ignore]
    fn file_icon_preview() {
        crate::preview::render("file-icon-preview", [176, 112], 1.0, |root| {
            let rect = root.max_rect();
            root.painter()
                .rect_filled(rect, 6.0, Color32::from_rgb(20, 21, 24));
            draw_file_icon(root.painter(), rect, Path::new("report.pdf"));
        });
    }
}
