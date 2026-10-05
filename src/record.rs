//! Screen recording of a region: frames from xcap's per-monitor recorder,
//! cropped and piped as raw RGBA into an `ffmpeg` process that encodes them.
//! With audio, the video goes to a temporary file and the sound (see
//! `audio`) is muxed in when the recording stops.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use image::RgbaImage;
use xcap::{Frame, Monitor, VideoRecorder};

use crate::audio::{Capture, Clock, Source};
use crate::capture::Rect;
use crate::cursor;

/// H.264 encoders to try, best first. All of them can be absent from a
/// given FFmpeg build; `mpeg4` is in practically every one.
const ENCODERS: &[&str] = &[
    "libx264",
    "h264_videotoolbox",
    "h264_mf",
    "libopenh264",
    "mpeg4",
];

/// How long a closed window can stay on screen; the first frame is taken
/// after that so the capture overlay isn't in it.
const OVERLAY_SETTLE: Duration = Duration::from_millis(60);

/// xcap's recorder threads never exit (stopping only pauses them), so each
/// monitor's recorder is kept and reused rather than started again.
type Recorders = HashMap<u32, (VideoRecorder, Receiver<Frame>)>;
static RECORDERS: Mutex<Option<Recorders>> = Mutex::new(None);

pub struct Recording {
    /// What's being recorded, in global physical pixels (the selection
    /// clipped to one monitor and rounded down to even sizes).
    pub rect: Rect,
    flags: Arc<Flags>,
    /// Recording time, which the audio follows.
    clock: Arc<Mutex<Clock>>,
    thread: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct Flags {
    stop: AtomicBool,
    pause: AtomicBool,
    /// Throw the recording away instead of saving it.
    abort: AtomicBool,
}

/// A recorded audio track: raw f32 file, sample rate, channels.
type Track = (PathBuf, u32, u16);

/// A finished recording.
#[derive(Debug)]
pub struct Recorded {
    pub path: PathBuf,
    /// The first frame, for a preview.
    pub first_frame: RgbaImage,
    /// Saved, but not quite as asked (e.g. without sound).
    pub warning: Option<String>,
}

/// Called with the saved recording, `None` if it was aborted.
pub type Done = Box<dyn FnOnce(Result<Option<Recorded>, String>) + Send>;

impl Recording {
    /// Starts recording `rect` (global physical pixels) to `out` at `fps`.
    /// `done` is called from the recording thread once the file is written.
    pub fn start(
        rect: Rect,
        fps: u32,
        ffmpeg: &str,
        audio: &[Source],
        show_cursor: bool,
        out: PathBuf,
        done: Done,
    ) -> Result<Self, String> {
        // The capture overlay was just closed; it stays on screen until the
        // compositor's next frame or so.
        let overlay_gone = Instant::now() + OVERLAY_SETTLE;
        let (monitor, region, origin) = monitor_for(rect)?;
        let ffmpeg = if ffmpeg.trim().is_empty() {
            "ffmpeg".to_string()
        } else {
            ffmpeg.trim().to_string()
        };
        let encoder = pick_encoder(&ffmpeg)?;
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("couldn't create {}: {e}", dir.display()))?;
        }
        thread::sleep(overlay_gone.saturating_duration_since(Instant::now()));
        let first = monitor
            .capture_region(region.x as u32, region.y as u32, region.w, region.h)
            .map_err(|e| format!("screen capture failed: {e}"))?
            .into_raw();
        let id = monitor.id().map_err(|e| e.to_string())?;
        let (recorder, frames) = take_recorder(&monitor, id)?;
        // Frames left over from the last recording on this monitor.
        while frames.try_recv().is_ok() {}
        recorder
            .start()
            .map_err(|e| format!("couldn't start recording: {e}"))?;

        let first_frame = RgbaImage::from_raw(region.w, region.h, first.clone())
            .ok_or("the first frame has the wrong size")?;

        // Open the audio devices before the video starts, so a missing
        // microphone stops the recording before it begins.
        let clock = Arc::new(Mutex::new(Clock::default()));
        let temp = temp_base();
        let mut captures = Vec::new();
        for (i, source) in audio.iter().enumerate() {
            let path = temp.with_extension(format!("{i}.f32"));
            match Capture::start(source, path, clock.clone()) {
                Ok(c) => captures.push(c),
                Err(e) => {
                    let _ = recorder.stop();
                    put_recorder(id, recorder, frames);
                    return Err(e);
                }
            }
        }
        // With sound, the video is muxed with it into `out` at the end.
        let video = if captures.is_empty() {
            out.clone()
        } else {
            temp.with_extension("mp4")
        };
        let mut child = match spawn_ffmpeg(&ffmpeg, encoder, region, fps, &video) {
            Ok(c) => c,
            Err(e) => {
                let _ = recorder.stop();
                put_recorder(id, recorder, frames);
                return Err(e);
            }
        };
        let stdin = child.stdin.take().expect("stdin is piped");
        let flags = Arc::new(Flags::default());
        let thread_flags = flags.clone();
        let thread_clock = clock.clone();
        let thread = thread::Builder::new()
            .name("record".into())
            .spawn(move || {
                // The pointer is drawn in at its global position, relative
                // to the region's top-left corner.
                let cursor_origin =
                    show_cursor.then_some((origin.0 + region.x, origin.1 + region.y));
                let result = pump(
                    &frames,
                    first,
                    region,
                    fps,
                    cursor_origin,
                    stdin,
                    &thread_flags,
                    &thread_clock,
                );
                let _ = recorder.stop();
                put_recorder(id, recorder, frames);
                let tracks: Vec<_> = captures
                    .into_iter()
                    .map(|c| {
                        let track = (c.path.clone(), c.sample_rate, c.channels);
                        (track, c.finish())
                    })
                    .collect();
                let remove_temps = |tracks: &[(Track, Result<(), String>)]| {
                    for ((path, ..), _) in tracks {
                        let _ = std::fs::remove_file(path);
                    }
                };
                if thread_flags.abort.load(Ordering::Relaxed) {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = std::fs::remove_file(&video);
                    remove_temps(&tracks);
                    return done(Ok(None));
                }
                if let Err(e) = finish(child, result) {
                    let _ = std::fs::remove_file(&video);
                    remove_temps(&tracks);
                    return done(Err(e));
                }
                let mut warning = None;
                if video != out {
                    let (good, failed): (Vec<_>, Vec<_>) =
                        tracks.iter().partition(|(_, r)| r.is_ok());
                    if let Some((_, Err(e))) = failed.first() {
                        warning = Some(format!("some audio wasn't recorded: {e}"));
                    }
                    let good: Vec<_> = good.into_iter().map(|(t, _)| t.clone()).collect();
                    if let Err(e) = mux(&ffmpeg, &video, &good, &out) {
                        warning = Some(format!("saved without sound: {e}"));
                        let _ = std::fs::remove_file(&out);
                        if let Err(e) = move_file(&video, &out) {
                            remove_temps(&tracks);
                            return done(Err(e));
                        }
                    }
                    let _ = std::fs::remove_file(&video);
                    remove_temps(&tracks);
                }
                crate::thumbnail::save_poster(&out, &first_frame);
                done(Ok(Some(Recorded {
                    path: out,
                    first_frame,
                    warning,
                })));
            })
            .map_err(|e| format!("couldn't start recording thread: {e}"))?;
        Ok(Self {
            rect: Rect {
                x: origin.0 + region.x,
                y: origin.1 + region.y,
                ..region
            },
            flags,
            clock,
            thread: Some(thread),
        })
    }

    /// Stops recording; the file is finished in the background.
    pub fn stop(&mut self) {
        self.flags.stop.store(true, Ordering::Relaxed);
        self.thread.take();
    }

    /// Stops recording and deletes what was recorded.
    pub fn abort(&mut self) {
        self.flags.abort.store(true, Ordering::Relaxed);
        self.stop();
    }

    pub fn set_paused(&self, paused: bool) {
        self.flags.pause.store(paused, Ordering::Relaxed);
        self.clock.lock().unwrap().set_paused(paused);
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        self.flags.stop.store(true, Ordering::Relaxed);
        // Let FFmpeg finish the file when snapr quits mid-recording.
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The monitor holding most of `rect`, `rect` clipped to it (relative to the
/// monitor, with an even width and height, as yuv420p needs) and the
/// monitor's top-left corner.
fn monitor_for(rect: Rect) -> Result<(Monitor, Rect, (i32, i32)), String> {
    let monitors = Monitor::all().map_err(|e| format!("couldn't list monitors: {e}"))?;
    let (monitor, clipped, origin) = monitors
        .into_iter()
        .filter_map(|m| {
            let bounds = Rect {
                x: m.x().ok()?,
                y: m.y().ok()?,
                w: m.width().ok()?,
                h: m.height().ok()?,
            };
            let clipped = rect.intersect(&bounds)?;
            Some((m, clipped, bounds))
        })
        .max_by_key(|(_, c, _)| c.w as u64 * c.h as u64)
        .map(|(m, c, b)| {
            (
                m,
                Rect {
                    x: c.x - b.x,
                    y: c.y - b.y,
                    ..c
                },
                (b.x, b.y),
            )
        })
        .ok_or("the region isn't on any monitor")?;
    let region = Rect {
        w: clipped.w & !1,
        h: clipped.h & !1,
        ..clipped
    };
    if region.w < 2 || region.h < 2 {
        return Err("the region is too small to record".into());
    }
    Ok((monitor, region, origin))
}

fn take_recorder(monitor: &Monitor, id: u32) -> Result<(VideoRecorder, Receiver<Frame>), String> {
    if let Some(r) = RECORDERS
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .remove(&id)
    {
        return Ok(r);
    }
    monitor
        .video_recorder()
        .map_err(|e| format!("screen recording isn't available: {e}"))
}

fn put_recorder(id: u32, recorder: VideoRecorder, frames: Receiver<Frame>) {
    RECORDERS
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(id, (recorder, frames));
}

/// The first encoder from `ENCODERS` this FFmpeg has.
fn pick_encoder(ffmpeg: &str) -> Result<&'static str, String> {
    let output = command(ffmpeg)
        .args(["-hide_banner", "-encoders"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| {
            format!(
                "FFmpeg is needed for recording but couldn't be run ({e}). Install it, or set its path under Settings \u{2192} General"
            )
        })?;
    let list = String::from_utf8_lossy(&output.stdout);
    ENCODERS
        .iter()
        .copied()
        .find(|enc| list.split_whitespace().any(|w| w == *enc))
        .ok_or_else(|| "this FFmpeg has no H.264 or MPEG-4 encoder".into())
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

fn spawn_ffmpeg(
    ffmpeg: &str,
    encoder: &str,
    region: Rect,
    fps: u32,
    out: &Path,
) -> Result<Child, String> {
    let mut cmd = command(ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
        .args(["-s", &format!("{}x{}", region.w, region.h)])
        .args(["-r", &fps.to_string(), "-i", "-"])
        .args(["-c:v", encoder]);
    match encoder {
        "libx264" => cmd.args(["-preset", "veryfast", "-crf", "23"]),
        "mpeg4" => cmd.args(["-q:v", "3"]),
        _ => cmd.args(["-b:v", "8M"]),
    };
    cmd.args(["-pix_fmt", "yuv420p", "-movflags", "+faststart"])
        .arg(out)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't start FFmpeg: {e}"))
}

/// Feeds FFmpeg one frame per tick until stopped, repeating the latest frame
/// when the screen hasn't changed (DXGI only reports changes). Time spent
/// paused is left out of the video. With `cursor_origin` (the region's
/// global top-left corner), the mouse pointer is drawn in. Starts `clock`
/// along with the video.
#[allow(clippy::too_many_arguments)]
fn pump(
    frames: &Receiver<Frame>,
    first: Vec<u8>,
    region: Rect,
    fps: u32,
    cursor_origin: Option<(i32, i32)>,
    mut stdin: ChildStdin,
    flags: &Flags,
    clock: &Mutex<Clock>,
) -> Result<(), String> {
    let interval = Duration::from_secs_f64(1.0 / fps.max(1) as f64);
    // The screen as last captured, and what's sent: that plus the pointer.
    let mut latest = first;
    let mut out = latest.clone();
    let mut cursors = cursor::Cursors::new();
    // FFmpeg takes a moment to start the encoder and blocks until it reads
    // the first frame. Time (the sound's too) starts once it has, or the
    // wait would be made up for by repeating that frame: a freeze.
    stdin
        .write_all(&latest)
        .map_err(|e| format!("FFmpeg stopped accepting frames: {e}"))?;
    let mut start = Instant::now();
    clock.lock().unwrap().start();
    let mut written: u64 = 1;
    while !flags.stop.load(Ordering::Relaxed) {
        if flags.pause.load(Ordering::Relaxed) {
            let paused_at = Instant::now();
            while flags.pause.load(Ordering::Relaxed) && !flags.stop.load(Ordering::Relaxed) {
                // Keep the channel from filling up with frames nobody wants.
                while frames.try_recv().is_ok() {}
                thread::sleep(Duration::from_millis(30));
            }
            start += paused_at.elapsed();
            continue;
        }
        if let Some(frame) = frames.try_iter().last() {
            crop_into(&frame, region, &mut latest);
        }
        let mut frame = &latest;
        if let Some(origin) = cursor_origin
            && let Some((at, image)) = cursors.current()
        {
            out.copy_from_slice(&latest);
            let at = (at.0 - origin.0, at.1 - origin.1);
            cursor::blend(&mut out, region.w, region.h, image, at);
            frame = &out;
        }
        // Catch up if encoding fell behind, so the video keeps real time.
        let due = (start.elapsed().as_secs_f64() / interval.as_secs_f64()) as u64 + 1;
        while written < due {
            stdin
                .write_all(frame)
                .map_err(|e| format!("FFmpeg stopped accepting frames: {e}"))?;
            written += 1;
        }
        let next = start + interval * written as u32;
        thread::sleep(next.saturating_duration_since(Instant::now()));
    }
    Ok(())
}

/// Copies `region` out of a whole-monitor RGBA frame.
fn crop_into(frame: &Frame, region: Rect, out: &mut [u8]) {
    let row = region.w as usize * 4;
    for y in 0..region.h as usize {
        let src_y = region.y as usize + y;
        if src_y >= frame.height as usize {
            break;
        }
        let start = (src_y * frame.width as usize + region.x as usize) * 4;
        let len = row.min((frame.width as usize * 4).saturating_sub(region.x as usize * 4));
        if let Some(src) = frame.raw.get(start..start + len) {
            out[y * row..y * row + len].copy_from_slice(src);
        }
    }
}

/// A unique path in the temp folder, without an extension.
fn temp_base() -> PathBuf {
    let tag: String = (0..8).map(|_| fastrand::alphanumeric()).collect();
    std::env::temp_dir().join(format!("snapr-{}-{tag}", std::process::id()))
}

/// Combines the silent video with the audio tracks (raw f32: path, sample
/// rate, channels), mixing them if there are several.
fn mux(ffmpeg: &str, video: &Path, tracks: &[Track], out: &Path) -> Result<(), String> {
    if tracks.is_empty() {
        return Err("no audio was recorded".into());
    }
    let mut cmd = command(ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(video);
    for (path, rate, channels) in tracks {
        cmd.args([
            "-f",
            "f32le",
            "-ar",
            &rate.to_string(),
            "-ac",
            &channels.to_string(),
            "-i",
        ])
        .arg(path);
    }
    if tracks.len() == 1 {
        cmd.args(["-map", "0:v", "-map", "1:a"]);
    } else {
        // Same format for each, then mixed without lowering the volume.
        let mut graph = String::new();
        for i in 1..=tracks.len() {
            graph += &format!("[{i}:a]aformat=sample_rates=48000:channel_layouts=stereo[a{i}];");
        }
        for i in 1..=tracks.len() {
            graph += &format!("[a{i}]");
        }
        graph += &format!(
            "amix=inputs={}:duration=longest:normalize=0[a]",
            tracks.len()
        );
        cmd.args(["-filter_complex", &graph, "-map", "0:v", "-map", "[a]"]);
    }
    let output = cmd
        .args(["-c:v", "copy", "-c:a", "aac", "-b:a", "160k", "-shortest"])
        .args(["-movflags", "+faststart"])
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

/// Closes FFmpeg's input and waits for it to write the file.
fn finish(mut child: Child, pumped: Result<(), String>) -> Result<(), String> {
    let mut errors = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        // stdin is already closed (dropped by `pump`), so FFmpeg is finishing.
        let _ = stderr.read_to_string(&mut errors);
    }
    let status = child
        .wait()
        .map_err(|e| format!("FFmpeg didn't finish: {e}"))?;
    pumped?;
    if status.success() {
        Ok(())
    } else {
        let detail = errors.lines().last().unwrap_or("no details").trim();
        Err(format!("FFmpeg failed ({status}): {detail}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records 1.5 s of frames that change every 1/60 s through FFmpeg and
    /// x264, and checks the video doesn't start with repeats of a frame (a
    /// freeze) while the encoder was starting up. Needs FFmpeg with libx264:
    /// `cargo test starts_without_a_freeze -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn starts_without_a_freeze() {
        let region = Rect {
            x: 0,
            y: 0,
            w: 1280,
            h: 720,
        };
        let fps = 30;
        let (tx, frames) = std::sync::mpsc::sync_channel(0);
        let stop = Arc::new(AtomicBool::new(false));
        let sender_stop = stop.clone();
        let sender = thread::spawn(move || {
            // A different shade every 1/60 s, like a playing video.
            let started = Instant::now();
            // Blocks until taken, like xcap's recorder.
            while !sender_stop.load(Ordering::Relaxed) {
                let n = (started.elapsed().as_secs_f64() * 60.0) as u32;
                let shade = (n * 7 % 256) as u8;
                let raw = vec![shade; (region.w * region.h * 4) as usize];
                if tx.send(Frame::new(region.w, region.h, raw)).is_err() {
                    break; // the pump is done
                }
            }
        });
        let out = temp_base().with_extension("mp4");
        let mut child = spawn_ffmpeg("ffmpeg", "libx264", region, fps, &out).unwrap();
        let stdin = child.stdin.take().unwrap();
        let flags = Arc::new(Flags::default());
        let clock = Arc::new(Mutex::new(Clock::default()));
        let (pump_flags, pump_clock) = (flags.clone(), clock.clone());
        // Taken as the recorder starts, so the same picture as the frame the
        // recorder is holding on to (shade 0) when the pump begins.
        let first = vec![0; (region.w * region.h * 4) as usize];
        let pumping = thread::spawn(move || {
            pump(&frames, first, region, fps, None, stdin, &pump_flags, &pump_clock)
        });
        thread::sleep(Duration::from_millis(1500));
        flags.stop.store(true, Ordering::Relaxed);
        let pumped = pumping.join().unwrap();
        stop.store(true, Ordering::Relaxed);
        sender.join().unwrap();
        finish(child, pumped).unwrap();

        // Each frame is one flat shade, so its average brightness says which
        // picture it is (x264 is lossy, so identical frames needn't decode
        // identically, but the shades are far enough apart).
        let decoded = command("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&out)
            .args(["-vf", "scale=8:8", "-pix_fmt", "gray", "-f", "rawvideo", "-"])
            .output()
            .unwrap();
        let _ = std::fs::remove_file(&out);
        let shades: Vec<f32> = decoded
            .stdout
            .chunks(64)
            .map(|f| f.iter().map(|&v| v as f32).sum::<f32>() / 64.0)
            .collect();
        println!("first shades: {:?}", &shades[..8.min(shades.len())]);
        let repeats_at_start = shades
            .windows(2)
            .take_while(|w| (w[0] - w[1]).abs() < 2.0)
            .count();
        println!(
            "{} frames ({:.2} s), first frame repeated {repeats_at_start} times",
            shades.len(),
            shades.len() as f64 / fps as f64
        );
        assert!(repeats_at_start == 0, "the video starts frozen");
    }

    #[test]
    fn crops_a_region_from_a_frame() {
        // 4x3 frame where each pixel's red channel is its index.
        let raw: Vec<u8> = (0..12u8).flat_map(|i| [i, 0, 0, 255]).collect();
        let frame = Frame::new(4, 3, raw);
        let region = Rect {
            x: 1,
            y: 1,
            w: 2,
            h: 2,
        };
        let mut out = vec![0; 2 * 2 * 4];
        crop_into(&frame, region, &mut out);
        let reds: Vec<u8> = out.chunks(4).map(|p| p[0]).collect();
        assert_eq!(reds, [5, 6, 9, 10]);
    }

    /// Records a small region for a second through a real FFmpeg:
    /// `cargo test record_round_trip -- --ignored`.
    #[test]
    #[ignore]
    fn record_round_trip() {
        let out = std::env::temp_dir().join("snapr-record-test.mp4");
        let _ = std::fs::remove_file(&out);
        let (tx, rx) = std::sync::mpsc::channel();
        let rect = Rect {
            x: 0,
            y: 0,
            w: 321,
            h: 201,
        };
        let mut rec = Recording::start(
            rect,
            30,
            "",
            &[],
            true,
            out.clone(),
            Box::new(move |r| {
                let _ = tx.send(r);
            }),
        )
        .unwrap();
        thread::sleep(Duration::from_secs(1));
        rec.stop();
        let path = rx
            .recv_timeout(Duration::from_secs(20))
            .unwrap()
            .unwrap()
            .expect("not aborted")
            .path;
        assert!(std::fs::metadata(&path).unwrap().len() > 1000);
    }

    /// Records two seconds with system audio, pausing in the middle:
    /// `cargo test record_with_sound -- --ignored`. Leaves the file in the
    /// temp folder to inspect.
    #[test]
    #[ignore]
    fn record_with_sound() {
        let out = std::env::temp_dir().join("snapr-sound-test.mp4");
        let _ = std::fs::remove_file(&out);
        let (tx, rx) = std::sync::mpsc::channel();
        let rect = Rect {
            x: 0,
            y: 0,
            w: 320,
            h: 200,
        };
        let mut rec = Recording::start(
            rect,
            30,
            "",
            &[Source::System],
            true,
            out.clone(),
            Box::new(move |r| {
                let _ = tx.send(r);
            }),
        )
        .unwrap();
        thread::sleep(Duration::from_secs(1));
        rec.set_paused(true);
        thread::sleep(Duration::from_millis(700));
        rec.set_paused(false);
        thread::sleep(Duration::from_secs(1));
        rec.stop();
        let recorded = rx
            .recv_timeout(Duration::from_secs(20))
            .unwrap()
            .unwrap()
            .expect("not aborted");
        assert_eq!(recorded.warning, None);
        assert!(std::fs::metadata(&recorded.path).unwrap().len() > 1000);
    }
}

#[cfg(all(test, windows))]
mod cursor_live {
    use super::*;

    /// Records a second around the mouse pointer to
    /// `%TEMP%/snapr-cursor-test.mp4`, to check it's drawn in the right
    /// place: `cargo test cursor_in_recording -- --ignored`.
    #[test]
    #[ignore]
    fn cursor_in_recording() {
        let (x, y) = crate::capture::cursor_position().expect("cursor position");
        let rect = Rect {
            x: x as i32 - 120,
            y: y as i32 - 120,
            w: 240,
            h: 240,
        };
        let out = std::env::temp_dir().join("snapr-cursor-test.mp4");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut rec = Recording::start(
            rect,
            30,
            "",
            &[],
            true,
            out,
            Box::new(move |r| {
                let _ = tx.send(r);
            }),
        )
        .unwrap();
        thread::sleep(Duration::from_secs(1));
        rec.stop();
        let recorded = rx
            .recv_timeout(Duration::from_secs(20))
            .unwrap()
            .unwrap()
            .unwrap();
        println!("recorded {}", recorded.path.display());
    }
}
