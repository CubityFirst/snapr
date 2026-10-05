//! Audio for screen recordings: what's playing (WASAPI loopback on Windows)
//! and/or a microphone. Each source's samples are padded to follow the
//! recording clock and sent on to be mixed and encoded with the video.

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
        let mut c = Clock::default();
        c.start();
        thread::sleep(Duration::from_millis(30));
        c.set_paused(true);
        let at_pause = c.elapsed();
        thread::sleep(Duration::from_millis(50));
        assert_eq!(c.elapsed(), at_pause);
        c.set_paused(false);
        thread::sleep(Duration::from_millis(20));
        let total = c.elapsed();
        assert!(
            total >= Duration::from_millis(50) && total < Duration::from_millis(90),
            "{total:?}"
        );
    }

    /// No data at all (silence on a loopback device) still gives a track as
    /// long as the recording.
    #[test]
    fn fills_silence() {
        let (tx, rx) = mpsc::channel::<Vec<f32>>();
        let clock = Mutex::new(Clock::default());
        clock.lock().unwrap().start();
        let stop = AtomicBool::new(false);
        let mut out = Vec::new();
        thread::scope(|s| {
            s.spawn(|| {
                thread::sleep(Duration::from_millis(250));
                stop.store(true, Ordering::Relaxed);
                drop(tx);
            });
            let collect = |samples: Vec<f32>| {
                out.extend(samples);
                Ok(())
            };
            write_samples(rx, collect, &clock, &stop, 1000, 2).unwrap();
        });
        let frames = out.len() / 2;
        assert!((200..=320).contains(&frames), "{frames} frames");
    }
}
