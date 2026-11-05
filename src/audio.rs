//! Audio for screen recordings: what's playing (WASAPI loopback on Windows,
//! the default sink's monitor on Linux) and/or a microphone. Each source's
//! samples are padded to follow the recording clock and sent on to be mixed
//! and encoded with the video.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};

/// Gaps longer than this (no data from the device) are filled with silence.
/// WASAPI loopback sends nothing at all while nothing is playing.
const GAP_TOLERANCE: Duration = Duration::from_millis(60);

#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// Everything the default output device plays.
    System,
    /// A microphone, by name; empty for the default one.
    Microphone(String),
}

/// Recording time that both video and audio follow: wall time minus pauses.
#[derive(Default)]
pub struct Clock {
    start: Option<Instant>,
    paused_at: Option<Instant>,
    paused_for: Duration,
}

impl Clock {
    /// Starts the clock now, if it isn't running yet.
    pub fn start(&mut self) {
        self.start.get_or_insert_with(Instant::now);
    }

    pub fn set_paused(&mut self, paused: bool) {
        match (paused, self.paused_at) {
            (true, None) => self.paused_at = Some(Instant::now()),
            (false, Some(t)) => {
                self.paused_for += t.elapsed();
                self.paused_at = None;
            }
            _ => {}
        }
    }

    pub fn paused(&self) -> bool {
        self.paused_at.is_some()
    }

    pub fn is_started(&self) -> bool {
        self.start.is_some()
    }

    pub fn elapsed(&self) -> Duration {
        let Some(start) = self.start else {
            return Duration::ZERO;
        };
        let now = self.paused_at.unwrap_or_else(Instant::now);
        now.duration_since(start).saturating_sub(self.paused_for)
    }
}

/// Interleaved f32 samples from the source with this index.
pub type Chunk = (usize, Vec<f32>);

/// One source being captured.
pub struct Capture {
    pub sample_rate: u32,
    pub channels: u16,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), String>>>,
}

impl Capture {
    /// Starts capturing `source`, sending its samples to `out` tagged with
    /// `index`, following `clock`.
    pub fn start(
        source: &Source,
        index: usize,
        out: Sender<Chunk>,
        clock: Arc<Mutex<Clock>>,
    ) -> Result<Self, String> {
        // cpal on Linux is ALSA, whose default device records the
        // microphone, even opened as the "output": what's playing comes from
        // the default sink's monitor instead, or not at all.
        #[cfg(target_os = "linux")]
        if *source == Source::System {
            return Self::start_monitor(index, out, clock);
        }
        let host = cpal::default_host();
        let device = match source {
            Source::System => host
                .default_output_device()
                .ok_or("no audio output device for system audio")?,
            Source::Microphone(name) if name.is_empty() => {
                host.default_input_device().ok_or("no microphone found")?
            }
            Source::Microphone(name) => host
                .input_devices()
                .map_err(|e| format!("couldn't list microphones: {e}"))?
                .find(|d| d.to_string() == *name)
                .ok_or_else(|| format!("microphone \"{name}\" isn't connected"))?,
        };
        // Output devices record in loopback mode with their output format.
        let supported = match source {
            Source::System => device.default_output_config(),
            Source::Microphone(_) => device.default_input_config(),
        }
        .map_err(|e| format!("audio device unavailable: {e}"))?;
        let (sample_rate, channels) = (supported.sample_rate(), supported.channels());

        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let (ready_tx, ready_rx) = mpsc::channel();
        // cpal streams aren't Send on every platform, so one thread builds,
        // runs and drops the stream.
        let thread = thread::Builder::new()
            .name("audio".into())
            .spawn(move || {
                let (tx, rx) = mpsc::channel::<Vec<f32>>();
                let stream = build_stream(&device, supported, tx);
                let stream =
                    match stream.and_then(|s| s.play().map(|()| s).map_err(|e| e.to_string())) {
                        Ok(s) => {
                            let _ = ready_tx.send(Ok(()));
                            s
                        }
                        Err(e) => {
                            let _ = ready_tx.send(Err(e.clone()));
                            return Err(e);
                        }
                    };
                let send = |samples: Vec<f32>| {
                    out.send((index, samples))
                        .map_err(|_| "the recording stopped taking audio".to_string())
                };
                let result = write_samples(
                    rx,
                    send,
                    &clock,
                    &stopping,
                    sample_rate,
                    channels,
                );
                drop(stream);
                result
            })
            .map_err(|e| format!("couldn't start audio thread: {e}"))?;
        ready_rx
            .recv()
            .map_err(|_| "audio thread stopped".to_string())??;
        Ok(Self {
            sample_rate,
            channels,
            stop,
            thread: Some(thread),
        })
    }

    /// `start` for what's playing on Linux: the default sink's monitor,
    /// through PipeWire's or PulseAudio's own recorder.
    #[cfg(target_os = "linux")]
    fn start_monitor(index: usize, out: Sender<Chunk>, clock: Arc<Mutex<Clock>>) -> Result<Self, String> {
        let (child, rx) = monitor::open()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let thread = thread::Builder::new()
            .name("audio".into())
            .spawn(move || {
                let send = |samples: Vec<f32>| {
                    out.send((index, samples))
                        .map_err(|_| "the recording stopped taking audio".to_string())
                };
                let (rate, channels) = (monitor::RATE, monitor::CHANNELS);
                let result = write_samples(rx, send, &clock, &stopping, rate, channels);
                drop(child);
                result
            })
            .map_err(|e| format!("couldn't start audio thread: {e}"))?;
        Ok(Self {
            sample_rate: monitor::RATE,
            channels: monitor::CHANNELS,
            stop,
            thread: Some(thread),
        })
    }

    /// Stops capturing, once the samples up to now are sent.
    pub fn finish(mut self) -> Result<(), String> {
        self.stop.store(true, Ordering::Relaxed);
        self.thread
            .take()
            .expect("finished once")
            .join()
            .map_err(|_| "audio thread panicked".to_string())?
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Names of the connected microphones, for the settings.
pub fn microphones() -> Vec<String> {
    cpal::default_host()
        .input_devices()
        .map(|devices| devices.map(|d| d.to_string()).collect())
        .unwrap_or_default()
}

/// What's playing on Linux: the default sink's monitor, recorded by
/// `pw-record` (PipeWire, told to capture a sink) or else `parec`
/// (PulseAudio, or PipeWire's stand-in for it). Never a microphone.
#[cfg(target_os = "linux")]
mod monitor {
    use std::io::Read;
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::thread;
    use std::time::{Duration, Instant};

    pub const RATE: u32 = 48_000;
    pub const CHANNELS: u16 = 2;
    /// How long a recorder that's sent nothing yet is given to fail (say,
    /// no sound server) before it's taken as working and just quiet.
    const STARTUP: Duration = Duration::from_millis(500);

    /// The recorders to try, in order, each writing raw interleaved f32
    /// samples to stdout.
    fn commands() -> [Command; 2] {
        let (rate, channels) = (RATE.to_string(), CHANNELS.to_string());
        let mut pw = Command::new("pw-record");
        pw.args(["--raw", "--format", "f32", "--rate", &rate, "--channels", &channels])
            .args(["--latency", "20ms", "-P"])
            .arg("{ stream.capture.sink = true node.name = snapr node.description = \"snapr (what's playing)\" }")
            .arg("-");
        let mut pa = Command::new("parec");
        pa.args(["--device=@DEFAULT_MONITOR@", "--raw", "--format=float32le"])
            .arg(format!("--rate={rate}"))
            .arg(format!("--channels={channels}"))
            .args(["--latency-msec=20", "--client-name=snapr"]);
        [pw, pa]
    }

    /// A recorder that's stopped once dropped.
    pub struct Recorder(Child);

    impl Drop for Recorder {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// Starts the first recorder that works, with its samples.
    pub fn open() -> Result<(Recorder, Receiver<Vec<f32>>), String> {
        let mut failures = Vec::new();
        for mut command in commands() {
            let name = command.get_program().to_string_lossy().into_owned();
            match start(&mut command) {
                Ok(started) => return Ok(started),
                Err(e) => failures.push(format!("{name}: {e}")),
            }
        }
        Err(format!(
            "couldn't record what's playing (it needs PipeWire's pw-record or PulseAudio's parec): {}",
            failures.join("; ")
        ))
    }

    fn start(command: &mut Command) -> Result<(Recorder, Receiver<Vec<f32>>), String> {
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| e.to_string())?;
        let stdout = child.stdout.take().expect("piped");
        let mut recorder = Recorder(child);
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        thread::Builder::new()
            .name("audio-monitor".into())
            .spawn(move || read_samples(stdout, tx, ready_tx))
            .map_err(|e| e.to_string())?;
        let began = Instant::now();
        loop {
            match ready_rx.recv_timeout(Duration::from_millis(20)) {
                Ok(()) => return Ok((recorder, rx)),
                // Its output closed before any sound.
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let status = recorder.0.wait().map_err(|e| e.to_string())?;
                    return Err(format!("stopped straight away ({status})"));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Some(status) = recorder.0.try_wait().map_err(|e| e.to_string())? {
                        return Err(format!("stopped straight away ({status})"));
                    }
                    if began.elapsed() >= STARTUP {
                        return Ok((recorder, rx));
                    }
                }
            }
        }
    }

    /// Sends `from`'s samples on as whole frames until it closes, saying
    /// on `ready` once the first arrive.
    fn read_samples(mut from: impl Read, to: Sender<Vec<f32>>, ready: Sender<()>) {
        let mut buf = vec![0u8; 16 * 1024];
        let mut pending = Vec::new();
        let mut ready = Some(ready);
        loop {
            let n = match from.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            pending.extend_from_slice(&buf[..n]);
            let samples = take_frames(&mut pending, CHANNELS);
            if samples.is_empty() {
                continue;
            }
            if let Some(r) = ready.take() {
                let _ = r.send(());
            }
            if to.send(samples).is_err() {
                return;
            }
        }
    }

    /// The whole frames of raw little-endian f32 samples at the start of
    /// `bytes`, leaving any partial one there for the next read.
    pub(super) fn take_frames(bytes: &mut Vec<u8>, channels: u16) -> Vec<f32> {
        let frame = 4 * channels.max(1) as usize;
        let whole = bytes.len() / frame * frame;
        let samples = bytes[..whole]
            .as_chunks::<4>().0.iter()
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        bytes.drain(..whole);
        samples
    }
}

fn build_stream(
    device: &cpal::Device,
    supported: cpal::SupportedStreamConfig,
    tx: mpsc::Sender<Vec<f32>>,
) -> Result<cpal::Stream, String> {
    let config = supported.config();
    let err = |e| eprintln!("audio stream error: {e}");
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => input::<f32>(device, config, tx, err),
        cpal::SampleFormat::I16 => input::<i16>(device, config, tx, err),
        cpal::SampleFormat::U16 => input::<u16>(device, config, tx, err),
        cpal::SampleFormat::I32 => input::<i32>(device, config, tx, err),
        other => return Err(format!("unsupported audio sample format {other}")),
    };
    stream.map_err(|e| format!("couldn't open the audio device: {e}"))
}

fn input<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    tx: mpsc::Sender<Vec<f32>>,
    err: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            let _ = tx.send(data.iter().map(|s| s.to_sample::<f32>()).collect());
        },
        err,
        None,
    )
}

/// Passes incoming samples on to `out`, dropping them while paused and
/// filling gaps with silence so the audio keeps pace with `clock`.
fn write_samples(
    rx: Receiver<Vec<f32>>,
    mut out: impl FnMut(Vec<f32>) -> Result<(), String>,
    clock: &Mutex<Clock>,
    stop: &AtomicBool,
    sample_rate: u32,
    channels: u16,
) -> Result<(), String> {
    let channels = channels.max(1) as u64;
    let tolerance = (GAP_TOLERANCE.as_secs_f64() * sample_rate as f64) as u64;
    let mut written: u64 = 0; // frames
    // Pads with silence up to `target` if it's more than `slack` frames ahead.
    let pad_to = |out: &mut dyn FnMut(Vec<f32>) -> Result<(), String>,
                  written: &mut u64,
                  target: u64,
                  slack: u64|
     -> Result<(), String> {
        if target > *written + slack {
            out(vec![0.0; ((target - *written) * channels) as usize])?;
            *written = target;
        }
        Ok(())
    };
    loop {
        let chunk = match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(c) => Some(c),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let (paused, now) = {
            let c = clock.lock().unwrap();
            // Sound from before the video starts is dropped.
            (c.paused() || !c.is_started(), c.elapsed())
        };
        let due = (now.as_secs_f64() * sample_rate as f64) as u64;
        if let Some(chunk) = chunk
            && !paused
        {
            let frames = chunk.len() as u64 / channels;
            // The chunk ends now, so silence fills whatever came before it.
            pad_to(
                &mut out,
                &mut written,
                due.saturating_sub(frames),
                tolerance,
            )?;
            out(chunk)?;
            written += frames;
        } else if !paused {
            pad_to(&mut out, &mut written, due, tolerance)?;
        }
        if stop.load(Ordering::Relaxed) {
            // Silence right up to the end, so the track is as long as the video.
            let end = (clock.lock().unwrap().elapsed().as_secs_f64() * sample_rate as f64) as u64;
            pad_to(&mut out, &mut written, end, 0)?;
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_leaves_out_pauses() {
        let ms = Duration::from_millis;
        let now = Instant::now();
        // Started 80 ms ago, and paused for the last 30.
        let mut c = Clock {
            start: Some(now - ms(80)),
            paused_at: Some(now - ms(30)),
            paused_for: Duration::ZERO,
        };
        assert_eq!(c.elapsed(), ms(50));
        c.set_paused(false);
        assert!(!c.paused());
        let total = c.elapsed();
        // However long that took, the 30 ms pause is left out.
        let since_start = now.elapsed() + ms(80);
        assert!(total >= ms(50) && total + ms(30) <= since_start, "{total:?}");
    }

    /// No data at all (silence on a loopback device) still gives a track as
    /// long as the recording.
    #[test]
    fn fills_silence() {
        // The sender stays open, as the stream's does until the recording ends.
        let (_tx, rx) = mpsc::channel::<Vec<f32>>();
        let clock = Mutex::new(Clock::default());
        clock.lock().unwrap().start();
        let stop = AtomicBool::new(false);
        let mut out = Vec::new();
        thread::scope(|s| {
            s.spawn(|| {
                thread::sleep(Duration::from_millis(250));
                stop.store(true, Ordering::Relaxed);
            });
            let collect = |samples: Vec<f32>| {
                out.extend(samples);
                Ok(())
            };
            write_samples(rx, collect, &clock, &stop, 1000, 2).unwrap();
        });
        // A frame a millisecond: silence from the start until it stopped.
        let frames = (out.len() / 2) as u128;
        let elapsed = clock.lock().unwrap().elapsed().as_millis();
        assert!(
            frames >= 250 && frames <= elapsed,
            "{frames} frames in {elapsed} ms"
        );
    }

    /// Raw samples split mid-frame keep the rest for the next read, so the
    /// channels stay in step.
    #[cfg(target_os = "linux")]
    #[test]
    fn monitor_samples_come_in_whole_frames() {
        let bytes: Vec<u8> = [1.0f32, -1.0, 0.5, 0.25].iter().flat_map(|s| s.to_le_bytes()).collect();
        // A frame and a half of stereo, then the rest.
        let mut pending = bytes[..12].to_vec();
        assert_eq!(monitor::take_frames(&mut pending, 2), [1.0, -1.0]);
        assert_eq!(pending.len(), 4);
        pending.extend_from_slice(&bytes[12..]);
        assert_eq!(monitor::take_frames(&mut pending, 2), [0.5, 0.25]);
        assert!(pending.is_empty());
    }

    /// Records two seconds of what's playing, leaving the raw samples (f32,
    /// interleaved) in the temp folder to inspect:
    /// `cargo test records_whats_playing -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn records_whats_playing() {
        let clock = Arc::new(Mutex::new(Clock::default()));
        let (tx, rx) = mpsc::channel();
        let capture = Capture::start(&Source::System, 0, tx, clock.clone()).unwrap();
        clock.lock().unwrap().start();
        thread::sleep(Duration::from_secs(2));
        let (rate, channels) = (capture.sample_rate, capture.channels);
        capture.finish().unwrap();
        let samples: Vec<f32> = rx.try_iter().flat_map(|(_, s)| s).collect();
        let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
        let path = std::env::temp_dir().join("snapr-whats-playing.f32");
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        std::fs::write(&path, bytes).unwrap();
        println!("{} frames at {rate} Hz x{channels}, peak {peak}, in {}", samples.len() / channels as usize, path.display());
        assert!(samples.len() / channels as usize >= rate as usize);
    }
}
