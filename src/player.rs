//! Plays a video inside the Recent page's preview. One thread decodes shrunk
//! frames (see `decode`), another the sound as 32-bit float samples for cpal
//! to play. Both follow a wall clock that starts once the first frame (and
//! sound) is ready, and seeking restarts them at the new position.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};

use crate::decode::{self, Info};

/// Largest frame to decode, in pixels.
const MAX_SIZE: (u32, u32) = (1600, 1000);
/// Decoded frames waiting to be shown.
const FRAME_QUEUE: usize = 4;
/// How much sound is decoded ahead.
const AUDIO_AHEAD: Duration = Duration::from_millis(500);
/// How long to wait for the sound before starting without it.
const AUDIO_WAIT: Duration = Duration::from_millis(400);

enum Frame {
    Picture(f64, egui::ColorImage),
    End,
    Failed(String),
}

/// The decoders for one stretch of playback, from `from` seconds on.
struct Streams {
    from: f64,
    frames: Receiver<Frame>,
    /// The next frame, once it's been taken off the queue but isn't due yet.
    next: Option<(f64, egui::ColorImage)>,
    ended: bool,
    /// A frame from these streams is on screen.
    shown: bool,
    audio: Option<Sound>,
    /// When `from` was (or would have been) on the wall clock; `None` until
    /// playback starts or while paused.
    started: Option<Instant>,
    spawned: Instant,
}

pub struct Player {
    pub path: PathBuf,
    ffmpeg: String,
    info: Option<Info>,
    probe: Option<Receiver<Option<Info>>>,
    streams: Option<Streams>,
    texture: Option<egui::TextureHandle>,
    /// Where playback is, in seconds.
    position: f64,
    paused: bool,
    /// Decoding failed; nothing more will play.
    failed: bool,
}

impl Player {
    /// Starts playing `path` from the beginning.
    pub fn new(path: PathBuf, ffmpeg: &str) -> Self {
        let ffmpeg = if ffmpeg.trim().is_empty() {
            "ffmpeg".to_string()
        } else {
            ffmpeg.trim().to_string()
        };
        let (tx, rx) = mpsc::channel();
        let (p, f) = (path.clone(), ffmpeg.clone());
        let _ = thread::Builder::new()
            .name("video-probe".into())
            .spawn(move || {
                let _ = tx.send(decode::probe(&p, &f));
            });
        Self {
            path,
            ffmpeg,
            info: None,
            probe: Some(rx),
            streams: None,
            texture: None,
            position: 0.0,
            paused: false,
            failed: false,
        }
    }

    pub fn duration(&self) -> Option<f64> {
        self.info.and_then(|i| i.duration)
    }

    pub fn position(&self) -> f64 {
        self.position
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn failed(&self) -> bool {
        self.failed
    }

    /// At the end of the video (or stopped by an error).
    pub fn finished(&self) -> bool {
        self.failed || (self.info.is_some() && self.streams.is_none())
    }

    /// Still waiting for the first picture.
    pub fn loading(&self) -> bool {
        !self.failed && self.texture.is_none()
    }

    pub fn toggle(&mut self) {
        if self.finished() && !self.failed {
            self.seek(0.0);
            self.paused = false;
        } else {
            self.set_paused(!self.paused);
        }
    }

    pub fn set_paused(&mut self, paused: bool) {
        if paused == self.paused {
            return;
        }
        self.paused = paused;
        // `update` starts the clock again.
        if paused && let Some(s) = &mut self.streams {
            s.started = None;
            if let Some(a) = &s.audio {
                a.playing.store(false, Ordering::Relaxed);
            }
        }
    }

    /// Jumps to `to` seconds; the picture stays until the new one is ready.
    pub fn seek(&mut self, to: f64) {
        let Some(info) = self.info else {
            return;
        };
        let to = match info.duration {
            Some(d) => to.clamp(0.0, (d - 0.05).max(0.0)),
            None => to.max(0.0),
        };
        self.position = to;
        self.streams = None;
        match start(&self.path, &self.ffmpeg, info, to) {
            Ok(s) => self.streams = Some(s),
            Err(e) => {
                eprintln!("couldn't play {}: {e}", self.path.display());
                self.failed = true;
            }
        }
    }

    /// Moves playback along and returns the picture to show now.
    pub fn update(&mut self, ctx: &egui::Context) -> Option<&egui::TextureHandle> {
        if let Some(rx) = &self.probe {
            match rx.try_recv() {
                Ok(Some(info)) => {
                    self.probe = None;
                    self.info = Some(info);
                    self.seek(0.0);
                }
                Ok(None) | Err(TryRecvError::Disconnected) => {
                    self.probe = None;
                    self.failed = true;
                }
                Err(TryRecvError::Empty) => ctx.request_repaint_after(Duration::from_millis(30)),
            }
        }
        let fps = self.info.map_or(30.0, |i| i.fps);
        let mut ended = false;
        if let Some(s) = &mut self.streams {
            // Take what's been decoded; the queue is short, so this doesn't
            // run ahead of the clock.
            if s.next.is_none() && !s.ended {
                match s.frames.try_recv() {
                    Ok(Frame::Picture(t, img)) => s.next = Some((t, img)),
                    Ok(Frame::End) | Err(TryRecvError::Disconnected) => s.ended = true,
                    Ok(Frame::Failed(e)) => {
                        eprintln!("couldn't play {}: {e}", self.path.display());
                        s.ended = true;
                        self.failed = !s.shown;
                    }
                    Err(TryRecvError::Empty) => {}
                }
            }
            // Start once there's something to show, giving the sound a
            // moment to catch up so they begin together.
            if s.started.is_none() && !self.paused && (s.next.is_some() || s.shown) {
                let sound_ready = s.audio.as_ref().is_none_or(|a| a.ready())
                    || s.spawned.elapsed() > AUDIO_WAIT;
                if sound_ready {
                    // Picks up from where it was paused.
                    s.started = Some(Instant::now() - secs(self.position - s.from));
                    if let Some(a) = &s.audio {
                        a.playing.store(true, Ordering::Relaxed);
                    }
                }
            }
            let now = match s.started {
                Some(t) => s.from + t.elapsed().as_secs_f64(),
                None => self.position,
            };
            // Show the latest frame that's due, skipping any that are late.
            // The first frame shows right away, so a seek updates the picture
            // even while paused.
            loop {
                let due = match &s.next {
                    Some((t, _)) => *t <= now || !s.shown,
                    None => false,
                };
                if !due {
                    break;
                }
                let (t, img) = s.next.take().unwrap();
                let shown_while_paused = s.started.is_none();
                s.shown = true;
                match &mut self.texture {
                    Some(tex) => tex.set(img, egui::TextureOptions::LINEAR),
                    None => {
                        self.texture =
                            Some(ctx.load_texture("video", img, egui::TextureOptions::LINEAR))
                    }
                }
                if shown_while_paused {
                    // Keep the paused position rather than the frame's,
                    // and leave the rest of the queue for when it plays.
                    break;
                }
                self.position = t;
                match s.frames.try_recv() {
                    Ok(Frame::Picture(t, img)) => s.next = Some((t, img)),
                    Ok(Frame::End | Frame::Failed(_)) | Err(TryRecvError::Disconnected) => {
                        s.ended = true
                    }
                    Err(TryRecvError::Empty) => {}
                }
            }
            if s.started.is_some() {
                self.position = self.position.max(now);
                if let Some(d) = self.info.and_then(|i| i.duration) {
                    self.position = self.position.min(d);
                }
            }
            if s.ended && s.next.is_none() {
                // Let the last frame have its time on screen.
                if s.started.is_none() || now >= self.position + 1.0 / fps {
                    ended = true;
                }
            }
            if !ended {
                // Wake for the next frame, or poll while the decoder catches up.
                let wait = match (&s.next, s.started) {
                    (Some((t, _)), Some(_)) => secs(t - now).min(Duration::from_millis(30)),
                    _ => Duration::from_millis(10),
                };
                if !self.paused || s.next.is_none() {
                    ctx.request_repaint_after(wait);
                }
            }
        }
        if ended {
            self.streams = None;
            self.paused = true;
            if let Some(d) = self.duration() {
                self.position = d;
            }
        }
        self.texture.as_ref()
    }
}

fn secs(s: f64) -> Duration {
    Duration::from_secs_f64(s.max(0.0))
}

/// Starts decoding from `from` seconds.
fn start(path: &Path, ffmpeg: &str, info: Info, from: f64) -> Result<Streams, String> {
    let (tx, frames) = mpsc::sync_channel(FRAME_QUEUE);
    let (p, f) = (path.to_owned(), ffmpeg.to_owned());
    thread::Builder::new()
        .name("video-frames".into())
        .spawn(move || {
            // Stops once the frames aren't wanted any more.
            let decoded = decode::pictures(&p, &f, info, from, MAX_SIZE, &mut |picture| {
                let size = [picture.image.width() as usize, picture.image.height() as usize];
                let image = egui::ColorImage::from_rgba_unmultiplied(size, picture.image.as_raw());
                tx.send(Frame::Picture(picture.time, image)).is_ok()
            });
            let _ = tx.send(match decoded {
                Ok(()) => Frame::End,
                Err(e) => Frame::Failed(e),
            });
        })
        .map_err(|e| e.to_string())?;
    // A video without sound, or no output device, just plays silently.
    let audio = Sound::start(path, ffmpeg, from)
        .map_err(|e| eprintln!("video sound: {e}"))
        .ok();
    Ok(Streams {
        from,
        frames,
        next: None,
        ended: false,
        shown: false,
        audio,
        started: None,
        spawned: Instant::now(),
    })
}

/// The video's sound, played on the default output device.
struct Sound {
    /// Samples decoded but not yet played. The decoding thread stops once
    /// it holds the only reference.
    buffer: Arc<Mutex<VecDeque<f32>>>,
    /// Decoding is done (the buffer may still have some left).
    done: Arc<AtomicBool>,
    pub playing: Arc<AtomicBool>,
    /// Dropping it ends the thread that owns the cpal stream.
    _stop: mpsc::Sender<()>,
}

impl Sound {
    fn start(path: &Path, ffmpeg: &str, from: f64) -> Result<Self, String> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("no audio output device")?;
        let supported = device
            .default_output_config()
            .map_err(|e| format!("audio device unavailable: {e}"))?;
        let (rate, channels) = (supported.sample_rate(), supported.channels());

        let buffer = Arc::new(Mutex::new(VecDeque::new()));
        let done = Arc::new(AtomicBool::new(false));
        let playing = Arc::new(AtomicBool::new(false));
        let ahead = (AUDIO_AHEAD.as_secs_f64() * rate as f64) as usize * channels as usize;
        {
            let (buffer, done) = (buffer.clone(), done.clone());
            let (path, ffmpeg) = (path.to_owned(), ffmpeg.to_owned());
            thread::Builder::new()
                .name("video-sound".into())
                .spawn(move || {
                    let decoded = decode::sound(&path, &ffmpeg, from, rate, channels, &mut |samples| {
                        while buffer.lock().unwrap().len() > ahead {
                            if Arc::strong_count(&buffer) == 1 {
                                return false;
                            }
                            thread::sleep(Duration::from_millis(10));
                        }
                        buffer.lock().unwrap().extend(samples);
                        true
                    });
                    if let Err(e) = decoded {
                        eprintln!("video sound: {e}");
                    }
                    done.store(true, Ordering::Relaxed);
                })
                .map_err(|e| e.to_string())?;
        }

        // cpal streams aren't Send everywhere, so a thread owns this one.
        let (stop, stopped) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel();
        {
            let (buffer, playing) = (buffer.clone(), playing.clone());
            thread::Builder::new()
                .name("video-output".into())
                .spawn(move || {
                    let stream = build_stream(&device, supported, buffer, playing)
                        .and_then(|s| s.play().map(|()| s).map_err(|e| e.to_string()));
                    let ok = stream.is_ok();
                    let _ = ready_tx.send(stream.as_ref().map(|_| ()).map_err(|e| e.clone()));
                    if ok {
                        let _ = stopped.recv(); // until the Sound is dropped
                    }
                    drop(stream);
                })
                .map_err(|e| e.to_string())?;
        }
        ready_rx.recv().map_err(|_| "audio thread stopped".to_string())??;
        Ok(Self {
            buffer,
            done,
            playing,
            _stop: stop,
        })
    }

    /// Has sound to play, or there's none coming.
    fn ready(&self) -> bool {
        !self.buffer.lock().unwrap().is_empty() || self.done.load(Ordering::Relaxed)
    }
}

fn build_stream(
    device: &cpal::Device,
    supported: cpal::SupportedStreamConfig,
    buffer: Arc<Mutex<VecDeque<f32>>>,
    playing: Arc<AtomicBool>,
) -> Result<cpal::Stream, String> {
    let config = supported.config();
    let err = |e| eprintln!("video sound error: {e}");
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => output::<f32>(device, config, buffer, playing, err),
        cpal::SampleFormat::I16 => output::<i16>(device, config, buffer, playing, err),
        cpal::SampleFormat::U16 => output::<u16>(device, config, buffer, playing, err),
        cpal::SampleFormat::I32 => output::<i32>(device, config, buffer, playing, err),
        other => return Err(format!("unsupported audio sample format {other}")),
    };
    stream.map_err(|e| format!("couldn't open the audio device: {e}"))
}

fn output<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    buffer: Arc<Mutex<VecDeque<f32>>>,
    playing: Arc<AtomicBool>,
    err: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            let silence = T::from_sample(0.0f32);
            if !playing.load(Ordering::Relaxed) {
                data.fill(silence);
                return;
            }
            let mut b = buffer.lock().unwrap();
            for s in data.iter_mut() {
                *s = b.pop_front().map_or(silence, T::from_sample);
            }
        },
        err,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plays a generated two-second clip with sound:
    /// `cargo test plays_a_clip -- --ignored`.
    #[test]
    #[ignore]
    fn plays_a_clip() {
        let dir = std::env::temp_dir().join("snapr-player-test");
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("clip.mp4");
        make_clip(&clip);

        let ctx = egui::Context::default();
        let mut player = Player::new(clip, "");
        let began = Instant::now();
        let mut first_frame = None;
        while !player.finished() && began.elapsed() < Duration::from_secs(6) {
            if player.update(&ctx).is_some() && first_frame.is_none() {
                first_frame = Some(began.elapsed());
            }
            thread::sleep(Duration::from_millis(5));
        }
        let took = began.elapsed();
        assert!(!player.failed());
        assert!(player.finished(), "still playing after {took:?}");
        assert!(first_frame.is_some());
        // The container's length can include the sound's last few ms.
        let duration = player.duration().unwrap();
        assert!((duration - 2.0).abs() < 0.05, "{duration}");
        // Real time, give or take startup.
        assert!(took > Duration::from_millis(1900) && took < Duration::from_millis(3500), "{took:?}");
        eprintln!("first frame after {first_frame:?}, done after {took:?}");

        // Seeking near the end finishes quickly.
        player.seek(1.5);
        player.set_paused(false);
        let began = Instant::now();
        while !player.finished() && began.elapsed() < Duration::from_secs(3) {
            player.update(&ctx);
            thread::sleep(Duration::from_millis(5));
        }
        assert!(began.elapsed() < Duration::from_millis(1500), "{:?}", began.elapsed());
    }

    /// Two seconds of a moving gradient at 25 fps with a 440 Hz tone.
    fn make_clip(path: &Path) {
        use crate::encode::{self, AudioFormat, VideoFormat};
        let (w, h, fps, rate) = (320u32, 240u32, 25u32, 48_000u32);
        let video = VideoFormat {
            width: w,
            height: h,
            fps,
        };
        let audio = AudioFormat {
            sample_rate: rate,
            channels: 2,
        };
        let mut enc = encode::open(path, video, Some(audio), "").unwrap();
        for n in 0..2 * fps {
            let frame: Vec<u8> = (0..w * h)
                .flat_map(|i| [((i % w + n * 8) % 256) as u8, (i / w) as u8, 128, 255])
                .collect();
            enc.video(&frame).unwrap();
            let tone: Vec<f32> = (0..rate / fps)
                .map(|i| (n * (rate / fps) + i) as f32 / rate as f32)
                .flat_map(|t| {
                    let s = (t * 440.0 * std::f32::consts::TAU).sin() * 0.2;
                    [s, s]
                })
                .collect();
            enc.audio(&tone).unwrap();
        }
        enc.finish().unwrap();
    }
}
