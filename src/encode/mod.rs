//! Encoding recordings to MP4 (H.264 + AAC) with what the OS provides:
//! Media Foundation on Windows, AVFoundation on macOS. Linux has no system
//! encoder, so it pipes the frames to FFmpeg. WebM (VP9 + Opus) is written
//! on Windows (the VP9 Video Extensions' encoder) and Linux (FFmpeg).
//!
//! Video arrives as RGBA frames at a fixed rate and sound as one stream of
//! interleaved stereo f32 samples (already mixed, see `mix`), both following
//! the recording clock, so frame and sample counts are the timestamps.

#[cfg(target_os = "macos")]
mod avf;
mod faststart;
// Compiled everywhere so its tests can run on any machine with FFmpeg.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod ffmpeg;
#[cfg(windows)]
mod mf;
#[cfg(windows)]
mod mf_webm;
pub mod mix;
#[cfg(windows)]
mod opus;
#[cfg(windows)]
mod vp9;
mod webm;

use std::path::Path;

#[derive(Debug, Clone, Copy)]
pub struct VideoFormat {
    /// Even, as 4:2:0 chroma needs.
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

pub trait Encoder {
    /// Appends a frame (RGBA, `width * height * 4` bytes) lasting 1/fps s.
    fn video(&mut self, rgba: &[u8]) -> Result<(), String>;
    /// Appends interleaved samples right after the previous ones. Ignored
    /// without an audio track.
    fn audio(&mut self, samples: &[f32]) -> Result<(), String>;
    /// Writes out the file. `Ok(Some(..))` means it was saved, but not quite
    /// as asked (e.g. without sound). Dropping instead leaves an unfinished
    /// file to delete.
    fn finish(self: Box<Self>) -> Result<Option<String>, String>;
}

/// The display an HDR recording is made on, for its metadata.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HdrInfo {
    pub peak_nits: f32,
    pub sdr_white_nits: f32,
}

/// Starts an HDR10 MP4 (HEVC Main 10) at `out`, which takes P010 frames
/// (see `hdr::to_p010`) instead of RGBA. Needs a GPU encoder.
#[cfg(windows)]
pub fn open_hdr(out: &Path, video: VideoFormat, audio: Option<AudioFormat>, hdr: HdrInfo) -> Result<Box<dyn Encoder>, String> {
    Ok(Box::new(mf::MediaFoundation::open_hdr(out, video, audio, hdr)?))
}

/// Starts an AV1 MP4 at `out`. Needs a GPU encoder.
#[cfg(windows)]
pub fn open_av1(out: &Path, video: VideoFormat, audio: Option<AudioFormat>) -> Result<Box<dyn Encoder>, String> {
    Ok(Box::new(mf::MediaFoundation::open_av1(out, video, audio)?))
}

/// Whether `out` is to be WebM rather than MP4 (by its extension).
pub fn is_webm(out: &Path) -> bool {
    out.extension().is_some_and(|e| e.eq_ignore_ascii_case("webm"))
}

/// Whether recordings can be saved as WebM here.
pub fn webm_supported() -> bool {
    cfg!(any(windows, target_os = "linux"))
}

/// The sample rate the sound must have for `out`, if the format fixes one
/// (Opus in WebM is 48 kHz).
pub fn required_sample_rate(out: &Path) -> Option<u32> {
    is_webm(out).then_some(48_000)
}

/// Starts a video file at `out`: WebM if its extension says so, else MP4.
/// `ffmpeg` is the program to use where the OS has no encoder (Linux);
/// empty finds it on PATH.
pub fn open(
    out: &Path,
    video: VideoFormat,
    audio: Option<AudioFormat>,
    #[allow(unused_variables)] ffmpeg: &str,
) -> Result<Box<dyn Encoder>, String> {
    if is_webm(out) && !webm_supported() {
        return Err("WebM recordings aren't available on this system".into());
    }
    #[cfg(windows)]
    if is_webm(out) {
        return Ok(Box::new(mf_webm::MfWebm::open(out, video, audio)?));
    }
    #[cfg(windows)]
    return Ok(Box::new(mf::MediaFoundation::open(out, video, audio)?));
    #[cfg(target_os = "macos")]
    return Ok(Box::new(avf::AssetWriter::open(out, video, audio)?));
    #[cfg(not(any(windows, target_os = "macos")))]
    return Ok(Box::new(ffmpeg::Ffmpeg::open(ffmpeg, out, video, audio)?));
}

/// A bit rate that keeps text sharp: about 0.15 bits per pixel per frame.
pub fn video_bitrate(v: VideoFormat) -> u32 {
    let bits = v.width as f64 * v.height as f64 * v.fps as f64 * 0.15;
    bits.clamp(1_000_000.0, 50_000_000.0) as u32
}

/// RGBA to NV12 (a Y plane, then interleaved U/V at half resolution), BT.709
/// limited range. `out` holds `w * h * 3 / 2` bytes; `w` and `h` are even.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn rgba_to_nv12(rgba: &[u8], w: usize, h: usize, out: &mut [u8]) {
    // Coefficients scaled by 2^16, including the 219/255 and 224/255 ranges.
    const ROUND: i32 = 1 << 15;
    let (y_plane, uv_plane) = out.split_at_mut(w * h);
    for (y, row) in y_plane.chunks_exact_mut(w).enumerate() {
        let src = &rgba[y * w * 4..(y + 1) * w * 4];
        for (dst, p) in row.iter_mut().zip(src.chunks_exact(4)) {
            let (r, g, b) = (p[0] as i32, p[1] as i32, p[2] as i32);
            *dst = (((11966 * r + 40254 * g + 4064 * b + ROUND) >> 16) + 16) as u8;
        }
    }
    for (cy, row) in uv_plane.chunks_exact_mut(w).enumerate() {
        let top = &rgba[2 * cy * w * 4..(2 * cy + 1) * w * 4];
        let bottom = &rgba[(2 * cy + 1) * w * 4..(2 * cy + 2) * w * 4];
        for (cx, uv) in row.chunks_exact_mut(2).enumerate() {
            let i = cx * 8;
            // Sum of the 2x2 block; divided by 4 with the coefficients below.
            let sum = |c: usize| {
                top[i + c] as i32 + top[i + 4 + c] as i32 + bottom[i + c] as i32 + bottom[i + 4 + c] as i32
            };
            let (r, g, b) = (sum(0), sum(1), sum(2));
            uv[0] = (((-6598 * r - 22187 * g + 28785 * b + 4 * ROUND) >> 18) + 128) as u8;
            uv[1] = (((28785 * r - 26147 * g - 2638 * b + 4 * ROUND) >> 18) + 128) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nv12_of(rgb: [u8; 3]) -> (u8, u8, u8) {
        let rgba: Vec<u8> = (0..4).flat_map(|_| [rgb[0], rgb[1], rgb[2], 255]).collect();
        let mut out = [0u8; 6];
        rgba_to_nv12(&rgba, 2, 2, &mut out);
        assert!(out[..4].iter().all(|&y| y == out[0]));
        (out[0], out[4], out[5])
    }

    #[test]
    fn nv12_matches_bt709() {
        assert_eq!(nv12_of([0, 0, 0]), (16, 128, 128));
        assert_eq!(nv12_of([255, 255, 255]), (235, 128, 128));
        // Reference values from the BT.709 equations, rounded.
        assert_eq!(nv12_of([255, 0, 0]), (63, 102, 240));
        assert_eq!(nv12_of([0, 255, 0]), (173, 42, 26));
        assert_eq!(nv12_of([0, 0, 255]), (32, 240, 118));
    }

    #[test]
    fn nv12_averages_chroma_over_2x2() {
        // Left column black, right column white: Y follows each pixel, the
        // chroma of the block stays neutral.
        let px = |v: u8| [v, v, v, 255];
        let rgba: Vec<u8> = [px(0), px(255), px(0), px(255)].concat();
        let mut out = [0u8; 6];
        rgba_to_nv12(&rgba, 2, 2, &mut out);
        assert_eq!(out, [16, 235, 16, 235, 128, 128]);
    }
}
