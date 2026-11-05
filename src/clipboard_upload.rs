//! What's on the clipboard, to upload: files copied in a file manager, an
//! image, or text (sent as a `.txt` file).

use std::path::PathBuf;

use image::RgbaImage;

/// Names text from the clipboard, for its key and in the uploads panel.
pub const TEXT_TITLE: &str = "Clipboard";

pub enum Contents {
    /// Files copied in a file manager.
    Files(Vec<PathBuf>),
    Image(RgbaImage),
    Text(String),
}

/// Reads the clipboard: files first, as a file manager can offer a picture
/// of them too, then an image (a browser's Copy image offers its address as
/// well), then text.
pub fn read() -> Result<Contents, String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| format!("upload failed: clipboard unavailable: {e}"))?;
    if let Ok(files) = clipboard.get().file_list() {
        let files: Vec<_> = files.into_iter().filter(|f| f.is_file()).collect();
        if !files.is_empty() {
            return Ok(Contents::Files(files));
        }
    }
    if let Ok(image) = clipboard.get_image() {
        return RgbaImage::from_raw(image.width as u32, image.height as u32, image.bytes.into_owned())
            .map(Contents::Image)
            .ok_or_else(|| "upload failed: couldn't read the image on the clipboard".into());
    }
    match clipboard.get_text() {
        Ok(text) if !text.trim().is_empty() => Ok(Contents::Text(text)),
        _ => Err("upload failed: there's nothing on the clipboard to upload".into()),
    }
}
