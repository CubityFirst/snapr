//! Linux (no system decoder): FFmpeg processes decode the video, one sending
//! shrunk frames as PPM pictures at a fixed rate, another the sound as
//! 32-bit float samples.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};

use image::RgbaImage;

use super::{Info, MAX_FPS, Picture};

/// Kills the process when dropped, so stopping early doesn't leave it
/// decoding.
struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn command(ffmpeg: &str) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(ffmpeg);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

/// Reads the video's length and frame rate from what `ffmpeg -i` prints.
pub fn probe(path: &Path, ffmpeg: &str) -> Option<Info> {
    let out = command(ffmpeg)
        .arg("-hide_banner")
        .arg("-i")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    parse_info(&String::from_utf8_lossy(&out.stderr))
}

fn parse_info(text: &str) -> Option<Info> {
    let duration = text.lines().find_map(|l| {
        let rest = l.trim().strip_prefix("Duration: ")?;
        let mut parts = rest.split(',').next()?.trim().split(':');
        let (h, m, s) = (parts.next()?, parts.next()?, parts.next()?);
        Some(h.parse::<f64>().ok()? * 3600.0 + m.parse::<f64>().ok()? * 60.0 + s.parse::<f64>().ok()?)
    });
    let video = text
        .lines()
        .find(|l| l.contains("Stream #") && l.contains("Video:"))?;
    // "29.97 fps", "30 tbr", "1k tbr".
    let rate = |unit: &str| {
        video.split(',').find_map(|part| {
            let n = part.trim().strip_suffix(unit)?.trim_end();
            match n.strip_suffix('k') {
                Some(k) => k.parse::<f64>().ok().map(|k| k * 1000.0),
                None => n.parse::<f64>().ok(),
            }
        })
    };
    let fps = rate("fps")
        .or_else(|| rate("tbr"))
        .filter(|f| *f > 0.0)
        .unwrap_or(30.0)
        .min(MAX_FPS);
    Some(Info { duration, fps })
}

pub fn pictures(
    path: &Path,
    ffmpeg: &str,
    info: Info,
    from: f64,
    max: (u32, u32),
    each: &mut dyn FnMut(Picture) -> bool,
) -> Result<(), String> {
    let filter = format!(
        "fps={},scale='min({},iw)':'min({},ih)':force_original_aspect_ratio=decrease",
        info.fps, max.0, max.1
    );
    let mut child = command(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin"])
        .args(["-ss", &format!("{from:.3}"), "-i"])
        .arg(path)
        .args(["-an", "-sn", "-vf", &filter, "-f", "image2pipe", "-c:v", "ppm", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("couldn't start FFmpeg: {e}"))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let _process = Process(child);
    let mut r = BufReader::with_capacity(1 << 20, stdout);
    let mut n = 0u64;
    while let Some(image) = read_ppm(&mut r) {
        let time = from + n as f64 / info.fps;
        n += 1;
        if !each(Picture { time, image }) {
            break;
        }
    }
    Ok(())
}

pub fn sound(
    path: &Path,
    ffmpeg: &str,
    from: f64,
    rate: u32,
    channels: u16,
    each: &mut dyn FnMut(Vec<f32>) -> bool,
) -> Result<bool, String> {
    let mut child = command(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin"])
        .args(["-ss", &format!("{from:.3}"), "-i"])
        .arg(path)
        .args(["-vn", "-sn", "-f", "f32le", "-ac", &channels.to_string()])
        .args(["-ar", &rate.to_string(), "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("couldn't start FFmpeg: {e}"))?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let _process = Process(child);
    let mut chunk = vec![0u8; 16 * 1024];
    let mut carry = Vec::new();
    let mut any = false;
    loop {
        let n = match stdout.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        carry.extend_from_slice(&chunk[..n]);
        let whole = carry.len() / 4 * 4;
        let samples = carry[..whole]
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        carry.drain(..whole);
        any = true;
        if !each(samples) {
            break;
        }
    }
    // FFmpeg writes nothing for a video without sound.
    Ok(any)
}

/// One binary PPM ("P6 width height 255", then RGB bytes).
fn read_ppm(r: &mut impl BufRead) -> Option<RgbaImage> {
    let mut fields = [0usize; 3];
    if token(r)? != "P6" {
        return None;
    }
    for f in &mut fields {
        *f = token(r)?.parse().ok()?;
    }
    let [w, h, max] = fields;
    if max != 255 || w == 0 || h == 0 {
        return None;
    }
    let mut rgb = vec![0u8; w * h * 3];
    r.read_exact(&mut rgb).ok()?;
    let rgba = rgb.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
    RgbaImage::from_raw(w as u32, h as u32, rgba)
}

/// A whitespace-separated header field, eating the one whitespace after it.
fn token(r: &mut impl BufRead) -> Option<String> {
    let mut s = String::new();
    let mut byte = [0u8];
    loop {
        r.read_exact(&mut byte).ok()?;
        if byte[0].is_ascii_whitespace() {
            if s.is_empty() {
                continue;
            }
            return Some(s);
        }
        s.push(byte[0] as char);
        if s.len() > 16 {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'clip.mp4':
  Duration: 00:01:02.50, start: 0.000000, bitrate: 1234 kb/s
  Stream #0:0[0x1](und): Video: h264 (High) (avc1 / 0x31637661), yuv420p(progressive), 1920x1080, 5000 kb/s, 29.97 fps, 29.97 tbr, 15360 tbn (default)
  Stream #0:1[0x2](und): Audio: aac (LC) (mp4a / 0x6134706D), 48000 Hz, stereo, fltp, 128 kb/s (default)
At least one output file must be specified";

    #[test]
    fn reads_ffmpeg_info() {
        let info = parse_info(SAMPLE).unwrap();
        assert_eq!(info.duration, Some(62.5));
        assert!((info.fps - 29.97).abs() < 1e-9);
    }

    #[test]
    fn falls_back_to_tbr_and_caps_fps() {
        let text = "  Duration: N/A, bitrate: N/A
  Stream #0:0: Video: vp9, yuv420p(tv), 640x480, 1k tbr, 1k tbn";
        let info = parse_info(text).unwrap();
        assert_eq!(info.duration, None);
        assert_eq!(info.fps, MAX_FPS);
    }

    #[test]
    fn no_video_stream() {
        assert_eq!(parse_info("  Stream #0:0: Audio: mp3, 44100 Hz"), None);
    }

    #[test]
    fn reads_ppm_frames() {
        let mut data = b"P6\n2 1\n255\n".to_vec();
        data.extend_from_slice(&[255, 0, 0, 0, 0, 255]);
        data.extend_from_slice(b"P6\n1 1\n255\n");
        data.extend_from_slice(&[1, 2, 3]);
        let mut r = BufReader::new(&data[..]);
        let a = read_ppm(&mut r).unwrap();
        assert_eq!(a.dimensions(), (2, 1));
        assert_eq!(a.get_pixel(1, 0).0, [0, 0, 255, 255]);
        assert_eq!(read_ppm(&mut r).unwrap().dimensions(), (1, 1));
        assert!(read_ppm(&mut r).is_none());
    }
}
