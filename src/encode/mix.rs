//! Mixes the recording's sound sources (each at its device's rate and
//! channel count) into one stereo stream for the encoder. Every source is
//! already padded with silence to follow the recording clock, so the
//! sources line up sample for sample once they're at the same rate.

use std::collections::VecDeque;

use super::AudioFormat;

pub struct Mixer {
    rate: u32,
    tracks: Vec<Track>,
}

struct Track {
    channels: u16,
    resampler: Option<Resampler>,
    /// Stereo samples at the output rate, not mixed yet.
    pending: VecDeque<f32>,
}

impl Mixer {
    /// One input per source: its sample rate and channel count. `rate` is
    /// the output rate if the format needs a particular one.
    pub fn new(inputs: &[AudioFormat], rate: Option<u32>) -> Self {
        // AAC encoders take 44.1 or 48 kHz; keep the sources' rate if they
        // agree on one of those, so nothing needs resampling.
        let rate = rate.unwrap_or_else(|| match inputs.first() {
            Some(f)
                if [44_100, 48_000].contains(&f.sample_rate)
                    && inputs.iter().all(|g| g.sample_rate == f.sample_rate) =>
            {
                f.sample_rate
            }
            _ => 48_000,
        });
        let tracks = inputs
            .iter()
            .map(|f| Track {
                channels: f.channels.max(1),
                resampler: (f.sample_rate != rate).then(|| Resampler::new(f.sample_rate, rate)),
                pending: VecDeque::new(),
            })
            .collect();
        Self { rate, tracks }
    }

    /// What `take` produces.
    pub fn format(&self) -> AudioFormat {
        AudioFormat {
            sample_rate: self.rate,
            channels: 2,
        }
    }

    /// Adds interleaved samples from source `track`.
    pub fn push(&mut self, track: usize, samples: &[f32]) {
        let Some(t) = self.tracks.get_mut(track) else {
            return;
        };
        let stereo = samples.chunks_exact(t.channels as usize).map(to_stereo);
        match &mut t.resampler {
            Some(r) => r.process(stereo, &mut t.pending),
            None => t.pending.extend(stereo.flatten()),
        }
    }

    /// The mix of what every source has delivered so far.
    pub fn take(&mut self) -> Vec<f32> {
        let ready = self.tracks.iter().map(|t| t.pending.len()).min().unwrap_or(0);
        self.mix(ready)
    }

    /// Everything left, at the end: sources that came up short are silent
    /// for the rest.
    pub fn take_rest(&mut self) -> Vec<f32> {
        let longest = self.tracks.iter().map(|t| t.pending.len()).max().unwrap_or(0);
        self.mix(longest)
    }

    /// Sums the first `len` pending samples of each source (missing ones
    /// count as silence), at full volume like FFmpeg's `amix normalize=0`.
    fn mix(&mut self, len: usize) -> Vec<f32> {
        let mut out = vec![0.0; len];
        for t in &mut self.tracks {
            let n = len.min(t.pending.len());
            for (o, s) in out.iter_mut().zip(t.pending.drain(..n)) {
                *o += s;
            }
        }
        out
    }
}

/// One frame of any channel count down to stereo, assuming the usual WAVE
/// order (FL FR FC LFE BL BR SL SR); the LFE channel is left out.
fn to_stereo(frame: &[f32]) -> [f32; 2] {
    match frame.len() {
        1 => [frame[0], frame[0]],
        2 => [frame[0], frame[1]],
        n => {
            const SIDE: f32 = std::f32::consts::FRAC_1_SQRT_2;
            let at = |i: usize| if i < n { frame[i] } else { 0.0 };
            let center = SIDE * at(2);
            let mut l = frame[0] + center + SIDE * (at(4) + at(6));
            let mut r = frame[1] + center + SIDE * (at(5) + at(7));
            // Keep a full-scale signal in every channel from clipping.
            let gain = 1.0
                / (1.0
                    + SIDE * [2, 4, 6].iter().filter(|&&i| i < n).count() as f32);
            l *= gain;
            r *= gain;
            [l, r]
        }
    }
}

/// Linear-interpolation resampler for stereo frames, kept across chunks.
struct Resampler {
    /// Input frames per output frame.
    step: f64,
    /// Position of the next output frame, in input frames counted from
    /// `prev` (0) and then the chunk being processed (1..).
    pos: f64,
    prev: [f32; 2],
}

impl Resampler {
    fn new(from: u32, to: u32) -> Self {
        Self {
            step: from as f64 / to as f64,
            pos: 1.0,
            prev: [0.0; 2],
        }
    }

    fn process(&mut self, input: impl Iterator<Item = [f32; 2]>, out: &mut VecDeque<f32>) {
        let input: Vec<[f32; 2]> = input.collect();
        let Some(&last) = input.last() else {
            return;
        };
        let n = input.len() as f64;
        let prev = self.prev;
        let at = |i: usize| if i == 0 { prev } else { input[i - 1] };
        while self.pos <= n {
            let i = self.pos.floor() as usize;
            let f = (self.pos - i as f64) as f32;
            let (a, b) = (at(i), if f > 0.0 { at(i + 1) } else { at(i) });
            out.push_back(a[0] + (b[0] - a[0]) * f);
            out.push_back(a[1] + (b[1] - a[1]) * f);
            self.pos += self.step;
        }
        self.pos -= n;
        self.prev = last;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(sample_rate: u32, channels: u16) -> AudioFormat {
        AudioFormat {
            sample_rate,
            channels,
        }
    }

    #[test]
    fn keeps_a_shared_rate() {
        assert_eq!(Mixer::new(&[fmt(44_100, 1)], None).format().sample_rate, 44_100);
        assert_eq!(Mixer::new(&[fmt(48_000, 2), fmt(44_100, 1)], None).format().sample_rate, 48_000);
        assert_eq!(Mixer::new(&[fmt(96_000, 2)], None).format().sample_rate, 48_000);
        assert_eq!(Mixer::new(&[fmt(44_100, 1)], Some(48_000)).format().sample_rate, 48_000);
    }

    #[test]
    fn mixes_mono_into_stereo() {
        let mut m = Mixer::new(&[fmt(48_000, 2), fmt(48_000, 1)], None);
        m.push(0, &[0.25, -0.25, 0.5, -0.5]);
        m.push(1, &[0.1]);
        // Only one frame from the microphone so far.
        assert_eq!(m.take(), [0.35, -0.15]);
        m.push(1, &[0.2, 0.3]);
        assert_eq!(m.take(), [0.7, -0.3]);
        // The microphone ran a frame longer; the other source is silent there.
        assert_eq!(m.take_rest(), [0.3, 0.3]);
    }

    #[test]
    fn resamples_to_the_right_length() {
        let mut m = Mixer::new(&[fmt(44_100, 1), fmt(48_000, 2)], None);
        // One second of each, in uneven chunks.
        for chunk in [441, 10_000, 33_659] {
            m.push(0, &vec![0.5; chunk]);
        }
        m.push(1, &vec![0.0; 96_000]);
        let out = m.take_rest();
        let frames = out.len() / 2;
        assert!((47_995..=48_000).contains(&frames), "{frames} frames");
        // A constant stays constant, up to where the resampled source ends
        // (a frame or so early; the mix is silent after that).
        assert!(out[..2 * 47_995].iter().all(|&s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn downmixes_surround() {
        let [l, r] = to_stereo(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        assert!(l > 0.0 && l <= 1.0 && r == 0.0);
        let [l, r] = to_stereo(&[1.0; 6]);
        assert!((l - 1.0).abs() < 1e-6 && (r - 1.0).abs() < 1e-6);
    }
}
