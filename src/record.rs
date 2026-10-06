//! Screen recording of a region: frames from xcap's per-monitor recorder,
//! cropped and handed to an encoder (see `encode`) along with the sound
//! (see `audio`), mixed as it arrives.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use image::RgbaImage;
use xcap::{Frame, Monitor, VideoRecorder};

use crate::audio::{Capture, Chunk, Clock, Source};
use crate::capture::Rect;
use crate::cursor;
use crate::encode::{self, Encoder, VideoFormat, mix::Mixer};

/// xcap's recorder threads never exit (stopping only pauses them), so each
/// monitor's recorder is kept and reused rather than started again.
type Recorders = HashMap<u32, (Recorder, Receiver<Frame>)>;
static RECORDERS: Mutex<Option<Recorders>> = Mutex::new(None);

/// xcap's recorder for one monitor. On macOS it holds an `AVCaptureSession`,
/// which objc2 doesn't mark `Send`, though AVFoundation lets a session be
/// started and stopped from any thread (Apple's samples do it on a
/// background queue).
struct Recorder(VideoRecorder);

// SAFETY: see above; each recorder is used by one thread at a time, moving
// between them through `RECORDERS`. It's `Send` anyway on Windows and Linux.
unsafe impl Send for Recorder {}

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

/// The sound sources' samples on their way to the encoder.
struct Sound {
    chunks: Receiver<Chunk>,
    mixer: Mixer,
}

impl Sound {
    /// The mix of what has arrived from every source.
    fn mixed(&mut self) -> Vec<f32> {
        for (track, samples) in self.chunks.try_iter() {
            self.mixer.push(track, &samples);
        }
        self.mixer.take()
    }
}

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
    /// With `hdr`, a display in HDR mode is recorded in HDR10 (Windows, MP4,
    /// with a GPU encoder; otherwise in SDR). With `av1`, an MP4 is AV1
    /// rather than H.264 (Windows, with a GPU encoder; otherwise H.264).
    /// `with_ffmpeg` encodes other recordings with FFmpeg rather than the
    /// system's encoder (Windows and macOS; Linux always uses it), falling
    /// back to the system's when FFmpeg can't be run.
    /// `done` is called from the recording thread once the file is written.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        rect: Rect,
        fps: u32,
        ffmpeg: &encode::FfmpegConfig,
        with_ffmpeg: bool,
        audio: &[Source],
        show_cursor: bool,
        #[allow(unused_variables)] hdr: bool,
        #[allow(unused_variables)] av1: bool,
        out: PathBuf,
        done: Done,
    ) -> Result<Self, String> {
        let (monitor, region, origin) = monitor_for(rect)?;
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("couldn't create {}: {e}", dir.display()))?;
        }
        #[cfg(windows)]
        let hdr = hdr
            && !encode::is_webm(&out)
            && crate::hdr::displays().iter().any(|d| (d.rect.x, d.rect.y) == origin);
        #[cfg(not(windows))]
        let hdr = false;
        // The usual capture starts right away, so frame 0 is the screen as
        // it is now. HDR capture starts on the recording thread (its
        // objects stay on one thread).
        let screen = if hdr { None } else { Some(ScreenFrames::start(&monitor, region)?) };

        // Open the audio devices before the video starts, so a missing
        // microphone stops the recording before it begins.
        let clock = Arc::new(Mutex::new(Clock::default()));
        let (chunk_tx, chunks) = mpsc::channel();
        let mut captures = Vec::new();
        for (i, source) in audio.iter().enumerate() {
            match Capture::start(source, i, chunk_tx.clone(), clock.clone()) {
                Ok(c) => captures.push(c),
                Err(e) => {
                    if let Some(s) = screen {
                        s.release();
                    }
                    return Err(e);
                }
            }
        }
        drop(chunk_tx);
        let sound = (!captures.is_empty()).then(|| {
            let formats: Vec<_> = captures
                .iter()
                .map(|c| encode::AudioFormat {
                    sample_rate: c.sample_rate,
                    channels: c.channels,
                })
                .collect();
            Sound {
                chunks,
                mixer: Mixer::new(&formats, encode::required_sample_rate(&out)),
            }
        });
        let format = VideoFormat {
            width: region.w,
            height: region.h,
            fps,
        };
        let ffmpeg = ffmpeg.clone();
        let with_ffmpeg = with_ffmpeg && cfg!(any(windows, target_os = "macos"));
        let flags = Arc::new(Flags::default());
        let thread_flags = flags.clone();
        let thread_clock = clock.clone();
        let (ready_tx, ready) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("record".into())
            .spawn(move || {
                // The encoder stays on this thread (Media Foundation is set
                // up per thread).
                let audio_format = sound.as_ref().map(|s| s.mixer.format());
                let mut warning = None;
                // HDR: the GPU encoder takes a second to start, so the
                // recording counts as started without waiting for it.
                #[cfg(windows)]
                let hdr_source = hdr.then(|| {
                    let _ = ready_tx.send(Ok(()));
                    open_hdr(origin, region, &out, format, audio_format)
                        .inspect_err(|e| warning = Some(format!("recorded in SDR: {e}")))
                        .ok()
                });
                #[cfg(not(windows))]
                let hdr_source: Option<Option<(Box<dyn Frames>, Box<dyn Encoder>, RgbaImage)>> = None;
                let opened = match hdr_source.flatten() {
                    Some(opened) => Ok(opened),
                    None => (|| {
                        let screen = match screen {
                            Some(s) => s,
                            // HDR didn't work out: the usual capture.
                            None => ScreenFrames::start(&monitor_for(rect)?.0, region)?,
                        };
                        let first = screen.picture();
                        #[cfg(windows)]
                        let av1_encoder = (av1 && !encode::is_webm(&out)).then(|| {
                            encode::open_av1(&out, format, audio_format)
                                .inspect_err(|e| warning = Some(format!("recorded in H.264: {e}")))
                                .ok()
                        });
                        #[cfg(not(windows))]
                        let av1_encoder: Option<Option<Box<dyn Encoder>>> = None;
                        let encoder = av1_encoder.flatten().or_else(|| {
                            with_ffmpeg.then(|| {
                                encode::open_ffmpeg(&out, format, audio_format, &ffmpeg)
                                    .inspect_err(|e| warning = Some(format!("recorded with the system's encoder: {e}")))
                                    .ok()
                            })?
                        });
                        let opened = match encoder {
                            Some(e) => Ok(e),
                            None => encode::open(&out, format, audio_format, &ffmpeg),
                        };
                        match opened {
                            Ok(e) => Ok((Box::new(screen) as Box<dyn Frames>, e, first)),
                            Err(e) => {
                                screen.release();
                                Err(e)
                            }
                        }
                    })(),
                };
                if !hdr {
                    let _ = ready_tx.send(opened.as_ref().map(drop).map_err(Clone::clone));
                }
                let (mut frames, mut encoder, first_frame) = match opened {
                    Ok(o) => o,
                    Err(e) => {
                        drop(captures);
                        if hdr {
                            done(Err(e));
                        }
                        return;
                    }
                };
                let mut sound = sound;
                // The pointer is drawn in at its global position, relative
                // to the region's top-left corner.
                let cursor_origin =
                    show_cursor.then_some((origin.0 + region.x, origin.1 + region.y));
                let result = pump(
                    frames.as_mut(),
                    fps,
                    cursor_origin,
                    encoder.as_mut(),
                    sound.as_mut(),
                    &thread_flags,
                    &thread_clock,
                );
                frames.release();
                // The sources pad their sound to the end of the recording
                // as they stop; then the last of it goes in.
                for capture in captures {
                    if let Err(e) = capture.finish() {
                        warning = Some(format!("some audio wasn't recorded: {e}"));
                    }
                }
                let result = result.and_then(|()| match &mut sound {
                    Some(s) => {
                        let mut rest = s.mixed();
                        rest.extend(s.mixer.take_rest());
                        encoder.audio(&rest)
                    }
                    None => Ok(()),
                });
                let discard = |encoder: Box<dyn Encoder>| {
                    drop(encoder);
                    let _ = std::fs::remove_file(&out);
                };
                if thread_flags.abort.load(Ordering::Relaxed) {
                    discard(encoder);
                    return done(Ok(None));
                }
                if let Err(e) = result {
                    discard(encoder);
                    return done(Err(e));
                }
                match encoder.finish() {
                    Ok(w) => warning = w.or(warning),
                    Err(e) => {
                        let _ = std::fs::remove_file(&out);
                        return done(Err(e));
                    }
                }
                crate::thumbnail::save_poster(&out, &first_frame);
                done(Ok(Some(Recorded {
                    path: out,
                    first_frame,
                    warning,
                })));
            })
            .map_err(|e| format!("couldn't start recording thread: {e}"))?;
        match ready.recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => return Err("the recording thread stopped".into()),
        }
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
        // Let the encoder finish the file when snapr quits mid-recording.
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

fn take_recorder(monitor: &Monitor, id: u32) -> Result<(Recorder, Receiver<Frame>), String> {
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
        .map(|(recorder, frames)| (Recorder(recorder), frames))
        .map_err(|e| format!("screen recording isn't available: {e}"))
}

fn put_recorder(id: u32, recorder: Recorder, frames: Receiver<Frame>) {
    RECORDERS
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(id, (recorder, frames));
}

/// Where a recording's pictures come from.
trait Frames {
    /// Takes in the newest picture of the screen, if it changed.
    fn update(&mut self) -> Result<(), String>;
    /// Lets go of what came in while paused.
    fn skip(&mut self);
    /// The frame to encode now, in the encoder's format, with the pointer
    /// drawn in at `cursor` (its top-left, relative to the region).
    fn frame(&mut self, cursor: Option<((i32, i32), &cursor::Image)>) -> &[u8];
    /// Stops capturing.
    fn release(self: Box<Self>) {}
}

/// xcap's per-monitor recorder, cropped to the region: RGBA.
struct ScreenFrames {
    /// The recorder and its monitor's id, to hand back when done (`None`
    /// when frames come from elsewhere, in tests).
    recorder: Option<(Recorder, u32)>,
    frames: Receiver<Frame>,
    region: Rect,
    /// The screen as last captured, and what's sent: that plus the pointer.
    latest: Vec<u8>,
    out: Vec<u8>,
}

impl ScreenFrames {
    /// Captures the region now (frame 0) and starts the recorder.
    fn start(monitor: &Monitor, region: Rect) -> Result<Self, String> {
        let first = monitor
            .capture_region(region.x as u32, region.y as u32, region.w, region.h)
            .map_err(|e| format!("screen capture failed: {e}"))?
            .into_raw();
        let id = monitor.id().map_err(|e| e.to_string())?;
        let (recorder, frames) = take_recorder(monitor, id)?;
        // Frames left over from the last recording on this monitor.
        while frames.try_recv().is_ok() {}
        if let Err(e) = recorder.0.start() {
            put_recorder(id, recorder, frames);
            return Err(format!("couldn't start recording: {e}"));
        }
        Ok(Self::new(Some((recorder, id)), frames, first, region))
    }

    fn new(recorder: Option<(Recorder, u32)>, frames: Receiver<Frame>, first: Vec<u8>, region: Rect) -> Self {
        Self {
            recorder,
            frames,
            region,
            out: first.clone(),
            latest: first,
        }
    }

    fn picture(&self) -> RgbaImage {
        RgbaImage::from_raw(self.region.w, self.region.h, self.latest.clone()).expect("region-sized")
    }

    fn release(self) {
        if let Some((recorder, id)) = self.recorder {
            let _ = recorder.0.stop();
            put_recorder(id, recorder, self.frames);
        }
    }
}

impl Frames for ScreenFrames {
    fn update(&mut self) -> Result<(), String> {
        if let Some(frame) = self.frames.try_iter().last() {
            crop_into(&frame, self.region, &mut self.latest);
        }
        Ok(())
    }

    fn skip(&mut self) {
        // Keep the channel from filling up with frames nobody wants.
        while self.frames.try_recv().is_ok() {}
    }

    fn frame(&mut self, cursor: Option<((i32, i32), &cursor::Image)>) -> &[u8] {
        match cursor {
            Some((at, image)) => {
                self.out.copy_from_slice(&self.latest);
                cursor::blend(&mut self.out, self.region.w, self.region.h, image, at);
                &self.out
            }
            None => &self.latest,
        }
    }

    fn release(self: Box<Self>) {
        ScreenFrames::release(*self)
    }
}

/// HDR capture (half floats), sent as P010 to an HDR encoder.
#[cfg(windows)]
struct HdrFrames {
    duplicator: crate::hdr::Duplicator,
    out: Vec<u8>,
}

#[cfg(windows)]
impl Frames for HdrFrames {
    fn update(&mut self) -> Result<(), String> {
        self.duplicator.poll(0).map(drop)
    }

    fn skip(&mut self) {
        let _ = self.duplicator.poll(0);
    }

    fn frame(&mut self, cursor: Option<((i32, i32), &cursor::Image)>) -> &[u8] {
        self.duplicator.p010(cursor, &mut self.out);
        &self.out
    }
}

/// Starts HDR capture of `region` on the display at `origin` and an HDR
/// encoder for it, with the first picture tone-mapped for a preview.
#[cfg(windows)]
fn open_hdr(
    origin: (i32, i32),
    region: Rect,
    out: &std::path::Path,
    format: VideoFormat,
    audio: Option<encode::AudioFormat>,
) -> Result<(Box<dyn Frames>, Box<dyn Encoder>, RgbaImage), String> {
    let display = crate::hdr::displays()
        .into_iter()
        .find(|d| (d.rect.x, d.rect.y) == origin)
        .ok_or("the display isn't in HDR mode any more")?;
    let duplicator = display.duplicate_region(region)?;
    let first = duplicator.sdr_picture();
    let info = encode::HdrInfo {
        peak_nits: duplicator.peak_nits,
        sdr_white_nits: duplicator.sdr_white_nits,
    };
    let encoder = encode::open_hdr(out, format, audio, info)?;
    let frames = HdrFrames {
        duplicator,
        out: Vec::new(),
    };
    Ok((Box::new(frames), encoder, first))
}

/// Feeds the encoder one frame per tick until stopped, repeating the latest
/// frame when the screen hasn't changed (DXGI only reports changes), and
/// with it the sound that has arrived. Time spent paused is left out of the
/// video. With `cursor_origin` (the region's global top-left corner), the
/// mouse pointer is drawn in. Starts `clock` along with the video.
fn pump(
    frames: &mut dyn Frames,
    fps: u32,
    cursor_origin: Option<(i32, i32)>,
    encoder: &mut dyn Encoder,
    mut sound: Option<&mut Sound>,
    flags: &Flags,
    clock: &Mutex<Clock>,
) -> Result<(), String> {
    let interval = Duration::from_secs_f64(1.0 / fps.max(1) as f64);
    let mut cursors = cursor::Cursors::new();
    // The pointer's image and where it goes, relative to the region.
    let pointer = |cursors: &mut cursor::Cursors| -> Option<((i32, i32), cursor::Image)> {
        let origin = cursor_origin?;
        let (at, image) = cursors.current()?;
        Some(((at.0 - origin.0, at.1 - origin.1), image.clone()))
    };
    // Encoders take a moment to start, mostly on their first frame. Time
    // (the sound's too) starts once that's done, or the wait would be made
    // up for by repeating that frame: a freeze.
    let p = pointer(&mut cursors);
    encoder.video(frames.frame(p.as_ref().map(|(at, i)| (*at, i))))?;
    let mut start = Instant::now();
    clock.lock().unwrap().start();
    let mut written: u64 = 1;
    while !flags.stop.load(Ordering::Relaxed) {
        if flags.pause.load(Ordering::Relaxed) {
            let paused_at = Instant::now();
            while flags.pause.load(Ordering::Relaxed) && !flags.stop.load(Ordering::Relaxed) {
                frames.skip();
                thread::sleep(Duration::from_millis(30));
            }
            start += paused_at.elapsed();
            continue;
        }
        frames.update()?;
        let p = pointer(&mut cursors);
        let frame = frames.frame(p.as_ref().map(|(at, i)| (*at, i)));
        // Catch up if encoding fell behind, so the video keeps real time.
        let due = (start.elapsed().as_secs_f64() / interval.as_secs_f64()) as u64 + 1;
        while written < due {
            encoder.video(frame)?;
            written += 1;
        }
        if let Some(sound) = &mut sound {
            encoder.audio(&sound.mixed())?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Records 1.5 s of frames that change every 1/60 s through the
    /// platform's encoder, and checks the video doesn't start with repeats of
    /// a frame (a freeze) while the encoder was starting up:
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
        // Opening audio devices and the encoder take a while, with the
        // recorder already running.
        thread::sleep(Duration::from_millis(100));
        let out = std::env::temp_dir().join("snapr-freeze-test.mp4");
        let format = VideoFormat {
            width: region.w,
            height: region.h,
            fps,
        };
        let flags = Arc::new(Flags::default());
        let clock = Arc::new(Mutex::new(Clock::default()));
        let (pump_flags, pump_clock) = (flags.clone(), clock.clone());
        // Taken as the recorder starts, so the same picture as the frame the
        // recorder is holding on to (shade 0) when the pump begins.
        let first = vec![0; (region.w * region.h * 4) as usize];
        let pump_out = out.clone();
        let pumping = thread::spawn(move || {
            let mut encoder = encode::open(&pump_out, format, None, &Default::default()).unwrap();
            let mut source = ScreenFrames::new(None, frames, first, region);
            pump(
                &mut source,
                fps,
                None,
                encoder.as_mut(),
                None,
                &pump_flags,
                &pump_clock,
            )
            .unwrap();
            encoder.finish().unwrap();
        });
        thread::sleep(Duration::from_millis(1500));
        flags.stop.store(true, Ordering::Relaxed);
        pumping.join().unwrap();
        stop.store(true, Ordering::Relaxed);
        sender.join().unwrap();

        // Each frame is one flat shade, so its average brightness says which
        // picture it is (H.264 is lossy, so identical frames needn't decode
        // identically, but the shades are far enough apart).
        let info = crate::decode::probe(&out, "").unwrap();
        let mut shades = Vec::new();
        crate::decode::pictures(&out, "", info, 0.0, (8, 8), &mut |p| {
            let pixels = p.image.pixels();
            let n = pixels.len() as f32;
            shades.push(pixels.map(|px| px[1] as f32).sum::<f32>() / n);
            true
        })
        .unwrap();
        let _ = std::fs::remove_file(&out);
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

    /// Records a small region for a second through the platform's encoder:
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
            &Default::default(),
            false,
            &[],
            true,
            false,
            false,
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
            &Default::default(),
            false,
            &[Source::System],
            true,
            false,
            false,
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

/// Records two seconds as WebM with system audio, pausing in the middle:
/// `cargo test record_webm_with_sound -- --ignored`. Leaves
/// `%TEMP%/snapr-sound-test.webm` to inspect.
#[cfg(all(test, any(windows, target_os = "linux")))]
mod webm_live {
    use super::*;

    #[test]
    #[ignore]
    fn record_webm_with_sound() {
        let out = std::env::temp_dir().join("snapr-sound-test.webm");
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
            &Default::default(),
            false,
            &[Source::System],
            true,
            false,
            false,
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

/// Records two seconds of the middle of the first display in HDR mode, if
/// any, to `%TEMP%/snapr-hdr-test.mp4`:
/// `cargo test record_hdr -- --ignored --nocapture`.
#[cfg(all(test, windows))]
mod hdr_live {
    use super::*;

    #[test]
    #[ignore]
    fn record_hdr() {
        // Like snapr itself (winit sets it); HDR capture needs it.
        unsafe {
            let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
                windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            );
        }
        let Some(display) = crate::hdr::displays().into_iter().next() else {
            println!("no display in HDR mode");
            return;
        };
        let d = display.rect;
        let rect = Rect {
            x: d.x + d.w as i32 / 2 - 320,
            y: d.y + d.h as i32 / 2 - 180,
            w: 640,
            h: 360,
        };
        let out = std::env::temp_dir().join("snapr-hdr-test.mp4");
        let _ = std::fs::remove_file(&out);
        let (tx, rx) = std::sync::mpsc::channel();
        let began = Instant::now();
        let mut rec = Recording::start(
            rect,
            30,
            &Default::default(),
            false,
            &[Source::System],
            true,
            true,
            false,
            out.clone(),
            Box::new(move |r| {
                let _ = tx.send(r);
            }),
        )
        .unwrap();
        println!("started in {:?}", began.elapsed());
        thread::sleep(Duration::from_secs(3));
        rec.stop();
        let recorded = rx
            .recv_timeout(Duration::from_secs(20))
            .unwrap()
            .unwrap()
            .expect("not aborted");
        println!("warning: {:?}", recorded.warning);
        println!("wrote {} ({} bytes)", recorded.path.display(), std::fs::metadata(&recorded.path).unwrap().len());
        assert_eq!(recorded.warning, None);
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
            &Default::default(),
            false,
            &[],
            true,
            false,
            false,
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
