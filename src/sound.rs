//! Feedback sounds, synthesized at startup (so there are no audio assets to
//! license) and played through each platform's built-in API.

use std::f32::consts::TAU;
use std::sync::LazyLock;

const RATE: u32 = 44_100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    /// A camera shutter, when a capture is taken.
    Capture,
    /// A soft chime, when something finished (e.g. copied).
    Done,
    /// A low two-tone, when something failed.
    Error,
}

static CAPTURE: LazyLock<Vec<u8>> = LazyLock::new(|| wav(&shutter()));
static DONE: LazyLock<Vec<u8>> = LazyLock::new(|| wav(&chime()));
static ERROR: LazyLock<Vec<u8>> = LazyLock::new(|| wav(&error()));

fn bytes(sound: Sound) -> &'static [u8] {
    match sound {
        Sound::Capture => &CAPTURE,
        Sound::Done => &DONE,
        Sound::Error => &ERROR,
    }
}

pub fn play(sound: Sound) {
    platform::play(sound, bytes(sound));
}

fn samples(seconds: f32) -> Vec<f32> {
    vec![0.0; (seconds * RATE as f32) as usize]
}

/// Deterministic white noise in -1..1.
fn noise(seed: u32) -> impl FnMut() -> f32 {
    let mut state = seed;
    move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state as f32 / u32::MAX as f32 * 2.0 - 1.0
    }
}

/// Two short mechanical clicks: the shutter opening and closing.
fn shutter() -> Vec<f32> {
    let mut out = samples(0.16);
    for (start, gain, brightness, seed) in [(0.0, 0.9, 0.55, 7), (0.065, 0.7, 0.7, 11)] {
        let mut rand = noise(seed);
        let (mut low, mut prev_in, mut high) = (0.0f32, 0.0f32, 0.0f32);
        let offset = (start * RATE as f32) as usize;
        for i in 0..(0.035 * RATE as f32) as usize {
            let t = i as f32 / RATE as f32;
            // Band-limit the noise: a one-pole low-pass, then a high-pass.
            let n = rand();
            low += brightness * (n - low);
            high = 0.92 * (high + low - prev_in);
            prev_in = low;
            let click = high * (-t / 0.006).exp();
            let thump = (TAU * 170.0 * t).sin() * (-t / 0.012).exp() * 0.5;
            if let Some(s) = out.get_mut(offset + i) {
                *s += gain * (click + thump);
            }
        }
    }
    out
}

/// A rising two-note chime.
fn chime() -> Vec<f32> {
    let mut out = samples(0.5);
    for (start, freq) in [(0.0, 880.0), (0.085, 1318.5)] {
        add_tone(&mut out, start, freq, 0.13, 0.3, 0.15);
    }
    out
}

/// A falling two-tone with a reedy edge.
fn error() -> Vec<f32> {
    let mut out = samples(0.4);
    for (start, freq) in [(0.0, 392.0), (0.13, 311.1)] {
        add_tone(&mut out, start, freq, 0.09, 0.3, 0.35);
    }
    out
}

/// Adds a decaying tone; `overtone` mixes in the third harmonic.
fn add_tone(out: &mut [f32], start: f32, freq: f32, decay: f32, gain: f32, overtone: f32) {
    let offset = (start * RATE as f32) as usize;
    for (i, s) in out.iter_mut().skip(offset).enumerate() {
        let t = i as f32 / RATE as f32;
        let attack = (t / 0.004).min(1.0);
        let env = attack * (-t / decay).exp();
        let wave = (TAU * freq * t).sin() + overtone * (TAU * freq * 3.0 * t).sin();
        *s += gain * env * wave / (1.0 + overtone);
    }
}

/// Encodes mono samples as a 16-bit PCM WAV file, with short fades at the
/// ends so it never starts or stops with a pop.
fn wav(samples: &[f32]) -> Vec<u8> {
    let fade = (0.002 * RATE as f32) as usize;
    let n = samples.len();
    let data_len = (n * 2) as u32;
    let mut out = Vec::with_capacity(44 + n * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for (i, s) in samples.iter().enumerate() {
        let edge = (i.min(n - 1 - i) as f32 / fade as f32).min(1.0);
        let v = (s * edge).clamp(-1.0, 1.0);
        out.extend_from_slice(&((v * i16::MAX as f32) as i16).to_le_bytes());
    }
    out
}

#[cfg(windows)]
mod platform {
    use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};

    pub fn play(_sound: super::Sound, wav: &'static [u8]) {
        // SAFETY: with SND_MEMORY the "name" is a pointer to the WAV data,
        // which is 'static so it outlives the asynchronous playback.
        unsafe {
            PlaySoundW(
                wav.as_ptr().cast(),
                std::ptr::null_mut(),
                SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
            );
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::cell::RefCell;

    use objc2::AllocAnyThread;
    use objc2::rc::Retained;
    use objc2_app_kit::NSSound;
    use objc2_foundation::NSData;

    thread_local! {
        /// The playing sound, kept alive until the next one starts.
        static PLAYING: RefCell<Option<Retained<NSSound>>> = const { RefCell::new(None) };
    }

    pub fn play(_sound: super::Sound, wav: &'static [u8]) {
        let Some(sound) = NSSound::initWithData(NSSound::alloc(), &NSData::with_bytes(wav)) else {
            return;
        };
        sound.play();
        PLAYING.with(|p| *p.borrow_mut() = Some(sound));
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use std::path::PathBuf;

    /// Writes the sound to a temporary file (once) and plays it with
    /// PulseAudio/PipeWire's `paplay`, or ALSA's `aplay`.
    pub fn play(sound: super::Sound, wav: &'static [u8]) {
        let path: PathBuf =
            std::env::temp_dir().join(format!("snapr-{sound:?}.wav").to_lowercase());
        if !path.exists() && std::fs::write(&path, wav).is_err() {
            return;
        }
        std::thread::spawn(move || {
            let played = std::process::Command::new("paplay")
                .arg(&path)
                .status()
                .is_ok_and(|s| s.success());
            if !played {
                let _ = std::process::Command::new("aplay")
                    .arg("-q")
                    .arg(&path)
                    .status();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sounds_are_valid_wavs_without_clipping() {
        for sound in [Sound::Capture, Sound::Done, Sound::Error] {
            let wav = bytes(sound);
            assert_eq!(&wav[..4], b"RIFF");
            assert_eq!(&wav[8..16], b"WAVEfmt ");
            let len = u32::from_le_bytes(wav[40..44].try_into().unwrap()) as usize;
            assert_eq!(wav.len(), 44 + len, "{sound:?}");
            let peak = wav[44..]
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs())
                .max()
                .unwrap();
            assert!(peak > 3000, "{sound:?} is nearly silent ({peak})");
            assert!(peak < i16::MAX as u16, "{sound:?} clips");
        }
    }

    /// Writes the sounds to `target/` to listen to: `cargo test write_sounds -- --ignored`.
    #[test]
    #[ignore]
    fn write_sounds() {
        for sound in [Sound::Capture, Sound::Done, Sound::Error] {
            let path = format!("{}/target/sound-{sound:?}.wav", env!("CARGO_MANIFEST_DIR"));
            std::fs::write(path, bytes(sound)).unwrap();
        }
    }
}
