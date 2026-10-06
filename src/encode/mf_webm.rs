//! Windows WebM recordings: VP9 from the VP9 Video Extensions' encoder,
//! Opus sound, written by `webm`.

use std::path::Path;

use super::opus::{self, Opus};
use super::vp9::{TICKS, Vp9};
use super::webm::{self, AudioTrack, VideoTrack};
use super::{AudioFormat, Encoder, VideoFormat, rgba_to_nv12};
use crate::mf::Session;

pub struct MfWebm {
    // Declared (and so dropped) before `_session`, which shuts MF down.
    vp9: Vp9,
    opus: Option<Opus>,
    writer: Option<webm::Writer>,
    video: VideoFormat,
    frames: u64,
    nv12: Vec<u8>,
    _session: Session,
}

impl MfWebm {
    /// `audio` must be 48 kHz, as Opus is.
    pub fn open(out: &Path, video: VideoFormat, audio: Option<AudioFormat>) -> Result<Self, String> {
        let session = Session::start()?;
        let vp9 = Vp9::open(video)?;
        let opus = match audio {
            Some(a) if a.sample_rate != opus::RATE => {
                return Err(format!("WebM sound must be 48 kHz, not {} Hz", a.sample_rate));
            }
            Some(a) => Some(Opus::new(a.channels)?),
            None => None,
        };
        let audio_track = match (&opus, audio) {
            (Some(o), Some(a)) => Some(AudioTrack {
                codec: "A_OPUS",
                sample_rate: a.sample_rate,
                channels: a.channels,
                codec_private: o.head(),
                codec_delay_ns: opus::PRE_SKIP as u64 * 1_000_000_000 / opus::RATE as u64,
                seek_pre_roll_ns: 80_000_000,
            }),
            _ => None,
        };
        let video_track = VideoTrack {
            codec: "V_VP9",
            width: video.width,
            height: video.height,
            fps: video.fps,
        };
        let writer = webm::Writer::create(out, Some(&video_track), audio_track.as_ref())
            .map_err(|e| format!("couldn't create {}: {e}", out.display()))?;
        Ok(Self {
            vp9,
            opus,
            writer: Some(writer),
            video,
            frames: 0,
            nv12: vec![0; video.width as usize * video.height as usize * 3 / 2],
            _session: session,
        })
    }

    fn writer(&mut self) -> &mut webm::Writer {
        self.writer.as_mut().expect("open until finished")
    }

    fn write_video(&mut self, packets: Vec<super::vp9::Packet>) -> Result<(), String> {
        for p in packets {
            self.writer()
                .video(&p.data, p.time / (TICKS / 1000), p.key)
                .map_err(|e| format!("couldn't write the video: {e}"))?;
        }
        Ok(())
    }

    fn write_audio(&mut self, packets: Vec<opus::Packet>) -> Result<(), String> {
        for p in packets {
            self.writer()
                .audio(&p.data, p.time_ms)
                .map_err(|e| format!("couldn't write the sound: {e}"))?;
        }
        Ok(())
    }
}

impl Encoder for MfWebm {
    fn video(&mut self, rgba: &[u8]) -> Result<(), String> {
        let (w, h, fps) = (
            self.video.width as usize,
            self.video.height as usize,
            self.video.fps.max(1) as u64,
        );
        rgba_to_nv12(rgba, w, h, &mut self.nv12);
        let time = self.frames * TICKS / fps;
        let duration = (self.frames + 1) * TICKS / fps - time;
        let packets = self.vp9.encode(&self.nv12, time, duration)?;
        self.frames += 1;
        self.write_video(packets)
    }

    fn audio(&mut self, samples: &[f32]) -> Result<(), String> {
        let Some(opus) = &mut self.opus else {
            return Ok(());
        };
        let packets = opus.push(samples)?;
        self.write_audio(packets)
    }

    fn finish(mut self: Box<Self>) -> Result<Option<String>, String> {
        let packets = self.vp9.finish()?;
        self.write_video(packets)?;
        if let Some(opus) = &mut self.opus {
            let packets = opus.finish()?;
            self.write_audio(packets)?;
        }
        let duration_ms = self.frames as f64 * 1000.0 / self.video.fps.max(1) as f64;
        self.writer
            .take()
            .expect("open until finished")
            .finish(duration_ms)
            .map_err(|e| format!("couldn't finish the video file: {e}"))?;
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records two seconds of a moving picture with a 440 Hz tone to
    /// `%TEMP%/snapr-webm-test.webm`: `cargo test webm_round_trip -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn webm_round_trip() {
        let out = std::env::temp_dir().join("snapr-webm-test.webm");
        let (w, h, fps) = (640u32, 360u32, 30u32);
        let video = VideoFormat {
            width: w,
            height: h,
            fps,
        };
        let audio = AudioFormat {
            sample_rate: 48_000,
            channels: 2,
        };
        let mut enc = super::super::open(&out, video, Some(audio), &Default::default()).unwrap();
        for n in 0..2 * fps {
            let frame: Vec<u8> = (0..w * h)
                .flat_map(|i| [((i % w + n * 8) % 256) as u8, (i / w % 256) as u8, 120, 255])
                .collect();
            enc.video(&frame).unwrap();
            let tone: Vec<f32> = (0..1600)
                .flat_map(|i| {
                    let t = (n * 1600 + i) as f32 / 48_000.0;
                    let s = (t * 440.0 * std::f32::consts::TAU).sin() * 0.25;
                    [s, s]
                })
                .collect();
            enc.audio(&tone).unwrap();
        }
        enc.finish().unwrap();
        println!("wrote {} ({} bytes)", out.display(), std::fs::metadata(&out).unwrap().len());
    }
}
