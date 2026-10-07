//! FFmpeg: on Linux (no system encoder), and on Windows and macOS when
//! chosen over the system's encoder. Frames are converted to NV12 (BT.709,
//! as the other encoders get them) and piped into an `ffmpeg` process. With
//! sound, the video goes to a temporary file and the mixed
//! sound to a raw f32 file, and FFmpeg combines them when it's finished.
//! MP4 is H.264 and AAC, WebM VP9 and Opus.

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};

use super::{AudioFormat, Encoder, FfmpegConfig, VideoFormat, is_webm, rgba_to_nv12, video_bitrate};
use crate::settings::FfmpegOptions;

/// H.264 encoders to try, best first. All of them can be absent from a
/// given FFmpeg build; `mpeg4` is in practically every one.
const ENCODERS: &[&str] = &[
    "libx264",
    "h264_videotoolbox",
    "h264_mf",
    "libopenh264",
    "mpeg4",
];

pub struct Ffmpeg {
    program: String,
    webm: bool,
    child: Child,
    stdin: Option<ChildStdin>,
    out: PathBuf,
    /// Where the video goes: `out`, or with sound a temporary file.
    video: PathBuf,
    sound: Option<Sound>,
    width: usize,
    height: usize,
    /// The frame being sent, as NV12.
    nv12: Vec<u8>,
}

/// The mixed sound, written raw while recording.
struct Sound {
    path: PathBuf,
    file: BufWriter<File>,
    format: AudioFormat,
}

impl Ffmpeg {
    pub fn open(
        config: &FfmpegConfig,
        out: &Path,
        video: VideoFormat,
        audio: Option<AudioFormat>,
    ) -> Result<Self, String> {
        let program = match config.program.trim() {
            "" => "ffmpeg".to_string(),
            p => p.to_string(),
        };
        let webm = is_webm(out);
        let encoder = pick_encoder(&program, webm)?;
        let (video_path, sound) = match audio {
            Some(format) => {
                let temp = temp_base();
                let path = temp.with_extension("f32");
                let file = File::create(&path)
                    .map_err(|e| format!("couldn't create {}: {e}", path.display()))?;
                let sound = Sound {
                    path,
                    file: BufWriter::new(file),
                    format,
                };
                (temp.with_extension(if webm { "webm" } else { "mp4" }), Some(sound))
            }
            None => (out.to_owned(), None),
        };
        let mut child = spawn(&program, encoder, &config.options, video, &video_path)?;
        let stdin = child.stdin.take();
        Ok(Self {
            program,
            webm,
            child,
            stdin,
            out: out.to_owned(),
            video: video_path,
            sound,
            width: video.width as usize,
            height: video.height as usize,
            nv12: vec![0; video.width as usize * video.height as usize * 3 / 2],
        })
    }
}

impl Encoder for Ffmpeg {
    fn video(&mut self, rgba: &[u8]) -> Result<(), String> {
        rgba_to_nv12(rgba, self.width, self.height, &mut self.nv12);
        self.stdin
            .as_mut()
            .expect("open until finished")
            .write_all(&self.nv12)
            .map_err(|e| format!("FFmpeg stopped accepting frames: {e}"))
    }

    fn audio(&mut self, samples: &[f32]) -> Result<(), String> {
        let Some(sound) = &mut self.sound else {
            return Ok(());
        };
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        sound
            .file
            .write_all(&bytes)
            .map_err(|e| format!("couldn't write audio: {e}"))
    }

    fn finish(mut self: Box<Self>) -> Result<Option<String>, String> {
        // Closing its input tells FFmpeg to write out the file.
        drop(self.stdin.take());
        let mut errors = String::new();
        if let Some(mut stderr) = self.child.stderr.take() {
            let _ = stderr.read_to_string(&mut errors);
        }
        let status = self
            .child
            .wait()
            .map_err(|e| format!("FFmpeg didn't finish: {e}"))?;
        if !status.success() {
            let detail = errors.lines().last().unwrap_or("no details").trim();
            return Err(format!("FFmpeg failed ({status}): {detail}"));
        }
        let Some(mut sound) = self.sound.take() else {
            return Ok(None);
        };
        let muxed = sound
            .file
            .flush()
            .map_err(|e| format!("couldn't write audio: {e}"))
            .and_then(|()| mux(&self.program, &self.video, &sound.path, sound.format, &self.out, self.webm));
        let mut warning = None;
        if let Err(e) = muxed {
            warning = Some(format!("saved without sound: {e}"));
            let _ = std::fs::remove_file(&self.out);
            move_file(&self.video, &self.out)?;
        }
        Ok(warning)
    }
}

impl Drop for Ffmpeg {
    fn drop(&mut self) {
        // Unfinished (aborted, or failed): stop FFmpeg and remove the
        // temporary files. `out` is the caller's to delete.
        if self.stdin.is_some() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(sound) = &self.sound {
            let _ = std::fs::remove_file(&sound.path);
        }
        if self.video != self.out {
            let _ = std::fs::remove_file(&self.video);
        }
    }
}

/// The first encoder from `ENCODERS` this FFmpeg has (for WebM, VP9).
fn pick_encoder(ffmpeg: &str, webm: bool) -> Result<&'static str, String> {
    let output = command(ffmpeg)
        .args(["-hide_banner", "-encoders"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| {
            format!(
                "FFmpeg couldn't be run ({e}). Install it, or set its path under Settings \u{2192} General"
            )
        })?;
    let list = String::from_utf8_lossy(&output.stdout);
    if webm {
        return list
            .split_whitespace()
            .any(|w| w == "libvpx-vp9")
            .then_some("libvpx-vp9")
            .ok_or_else(|| "this FFmpeg has no VP9 encoder (libvpx) for WebM".into());
    }
    ENCODERS
        .iter()
        .copied()
        .find(|enc| list.split_whitespace().any(|w| w == *enc))
        .ok_or_else(|| "this FFmpeg has no H.264 or MPEG-4 encoder".into())
}

/// Whether `program` (blank: `ffmpeg` on `PATH`) runs and can record
/// MP4, or with `webm` WebM: its version and the encoder it would use, or
/// what's wrong.
pub fn check(program: &str, webm: bool) -> Result<String, String> {
    let program = match program.trim() {
        "" => "ffmpeg",
        p => p,
    };
    let output = command(program)
        .arg("-version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("couldn't run {program}: {e}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let first = text.lines().next().unwrap_or_default();
    // "ffmpeg version 7.1-full_build-www.gyan.dev Copyright (c) ..."
    let version = match first.strip_prefix("ffmpeg version ") {
        Some(rest) if output.status.success() => {
            let full = rest.split_whitespace().next().unwrap_or("?");
            // "7.1-full_build-…" is 7.1; a git build ("N-11234-g…") stays whole.
            match full.split_once('-') {
                Some((number, _)) if number.starts_with(|c: char| c.is_ascii_digit()) => number,
                _ => full,
            }
        }
        _ => return Err(format!("{program} doesn't look like FFmpeg")),
    };
    let encoder = pick_encoder(program, webm)?;
    let format = if webm { "WebM" } else { "MP4" };
    Ok(match encoder {
        "libx264" | "libvpx-vp9" => format!("FFmpeg {version} works: {format} with {encoder}"),
        _ => format!("FFmpeg {version} works, but has no libx264: {format} with {encoder}, which is less sharp"),
    })
}

fn command(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// BT.709 limited range, what `rgba_to_nv12` makes; given for the input so
/// FFmpeg passes the colours through untouched, and for the output so
/// players read them right.
const COLOUR: &[&str] = &["-color_range", "tv", "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"];

fn spawn(ffmpeg: &str, encoder: &str, options: &FfmpegOptions, v: VideoFormat, out: &Path) -> Result<Child, String> {
    let extra = options.extra_args().map_err(|e| format!("FFmpeg arguments: {e}"))?;
    let mut cmd = command(ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "rawvideo", "-pix_fmt", "nv12"])
        .args(COLOUR)
        .args(["-s", &format!("{}x{}", v.width, v.height)])
        .args(["-r", &v.fps.to_string(), "-i", "-"])
        .args(["-c:v", encoder]);
    match encoder {
        "libx264" => cmd
            .args(["-preset", &options.x264_preset])
            .args(["-crf", &options.x264_crf.min(51).to_string()]),
        "mpeg4" => cmd.args(["-q:v", "3"]),
        // Real-time; VP9 is slow otherwise. Constant quality, up to the
        // usual bit rate.
        "libvpx-vp9" => cmd
            .args(["-deadline", "realtime", "-row-mt", "1"])
            .args(["-cpu-used", &options.vp9_speed.min(8).to_string()])
            .args(["-crf", &options.vp9_crf.min(63).to_string()])
            .args(["-b:v", &video_bitrate(v).to_string()]),
        _ => cmd.args(["-b:v", "8M"]),
    };
    cmd.args(["-pix_fmt", "yuv420p"]).args(COLOUR);
    if encoder != "libvpx-vp9" {
        cmd.args(["-movflags", "+faststart"]);
    }
    cmd.args(extra)
        .arg(out)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't start FFmpeg: {e}"))
}

/// A unique path in the temp folder, without an extension.
fn temp_base() -> PathBuf {
    let tag: String = (0..8).map(|_| fastrand::alphanumeric()).collect();
    std::env::temp_dir().join(format!("snapr-{}-{tag}", std::process::id()))
}

/// Combines the silent video with the raw f32 sound into `out` (AAC, or
/// Opus for WebM).
fn mux(ffmpeg: &str, video: &Path, sound: &Path, format: AudioFormat, out: &Path, webm: bool) -> Result<(), String> {
    let (codec, flags): (&[&str], &[&str]) = if webm {
        (&["-c:a", "libopus", "-b:a", "128k"], &[])
    } else {
        (&["-c:a", "aac", "-b:a", "160k"], &["-movflags", "+faststart"])
    };
    let output = command(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(video)
        .args(["-f", "f32le", "-ar", &format.sample_rate.to_string()])
        .args(["-ac", &format.channels.to_string(), "-i"])
        .arg(sound)
        .args(["-map", "0:v", "-map", "1:a"])
        .args(["-c:v", "copy"])
        .args(codec)
        .args(["-shortest"])
        .args(flags)
        .arg(out)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .output()
        .map_err(|e| format!("couldn't run FFmpeg: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let errors = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "FFmpeg couldn't add the sound: {}",
            errors.lines().last().unwrap_or("no details").trim()
        ))
    }
}

/// Renames, or copies across drives (the temp folder may be elsewhere).
fn move_file(from: &Path, to: &Path) -> Result<(), String> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to)
        .map(drop)
        .map_err(|e| format!("couldn't save {}: {e}", to.display()))?;
    let _ = std::fs::remove_file(from);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A missing program or one that isn't FFmpeg fails the check; the
    /// FFmpeg on `PATH`, if any, passes it.
    #[test]
    fn checks_the_path() {
        assert!(check("snapr-no-such-ffmpeg", false).unwrap_err().starts_with("couldn't run"));
        #[cfg(windows)]
        assert!(check("whoami", false).unwrap_err().contains("doesn't look like FFmpeg"));
        if command("ffmpeg").arg("-version").output().is_ok() {
            let msg = check("", false).unwrap();
            println!("{msg}");
            assert!(msg.starts_with("FFmpeg "), "{msg}");
        }
    }

    /// Encodes a few frames of one colour with sound, then decodes them
    /// back as NV12: the colours come through (give or take compression)
    /// and the file says BT.709. Skipped without FFmpeg.
    #[test]
    fn keeps_colours() {
        if command("ffmpeg").arg("-version").output().is_err() {
            eprintln!("skipped: no FFmpeg");
            return;
        }
        let (w, h) = (64, 48);
        let out = temp_base().with_extension("mp4");
        let video = VideoFormat { width: w, height: h, fps: 30 };
        let audio = AudioFormat { sample_rate: 48_000, channels: 2 };
        let config = FfmpegConfig::default();
        let mut enc: Box<dyn Encoder> = Box::new(Ffmpeg::open(&config, &out, video, Some(audio)).unwrap());
        let rgba: Vec<u8> = [200, 60, 30, 255].repeat((w * h) as usize);
        for _ in 0..15 {
            enc.video(&rgba).unwrap();
            enc.audio(&[0.0; 1600 * 2]).unwrap();
        }
        assert_eq!(enc.finish().unwrap(), None);

        let mut expected = vec![0; (w * h * 3 / 2) as usize];
        rgba_to_nv12(&rgba, w as usize, h as usize, &mut expected);
        let decoded = command("ffmpeg")
            .args(["-hide_banner", "-i"])
            .arg(&out)
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "nv12", "-"])
            .output()
            .unwrap();
        let _ = std::fs::remove_file(&out);
        let info = String::from_utf8_lossy(&decoded.stderr);
        assert!(info.contains("bt709"), "{info}");
        assert!(info.contains("Audio: aac"), "{info}");
        let near = |a: u8, b: u8| a.abs_diff(b) <= 2;
        let mid_uv = (w * h + w * h / 4 + w / 2) as usize;
        let (y, u, v) = (decoded.stdout[0], decoded.stdout[mid_uv], decoded.stdout[mid_uv + 1]);
        assert!(near(y, expected[0]) && near(u, expected[mid_uv]) && near(v, expected[mid_uv + 1]),
            "got {y} {u} {v}, expected {} {} {}", expected[0], expected[mid_uv], expected[mid_uv + 1]);
    }

    /// The CRF changes the size, and extra arguments (quoted ones too) reach
    /// FFmpeg. Skipped without FFmpeg.
    #[test]
    fn uses_options() {
        if command("ffmpeg").arg("-version").output().is_err() {
            eprintln!("skipped: no FFmpeg");
            return;
        }
        let (w, h) = (128, 96);
        let video = VideoFormat { width: w, height: h, fps: 30 };
        // Moving noise, so quality costs something.
        let frames: Vec<Vec<u8>> = (0..10)
            .map(|_| (0..w * h * 4).map(|_| fastrand::u8(..)).collect())
            .collect();
        let encode = |crf: u8, extra: &str| {
            let out = temp_base().with_extension("mp4");
            let config = FfmpegConfig {
                program: String::new(),
                options: FfmpegOptions { x264_crf: crf, extra_args: extra.into(), ..Default::default() },
            };
            let mut enc: Box<dyn Encoder> = Box::new(Ffmpeg::open(&config, &out, video, None).unwrap());
            for f in &frames {
                enc.video(f).unwrap();
            }
            enc.finish().unwrap();
            let size = std::fs::metadata(&out).unwrap().len();
            let info = command("ffmpeg").arg("-i").arg(&out).output().unwrap();
            let _ = std::fs::remove_file(&out);
            (size, String::from_utf8_lossy(&info.stderr).into_owned())
        };
        let (sharp, info) = encode(10, r#"-metadata "title=snapr options""#);
        let (rough, _) = encode(45, "");
        assert!(sharp > rough * 2, "CRF 10: {sharp} bytes, CRF 45: {rough}");
        assert!(info.contains("snapr options"), "{info}");
        assert!(FfmpegOptions { extra_args: r#"-metadata "title"#.into(), ..Default::default() }.extra_args().is_err());
    }
}
