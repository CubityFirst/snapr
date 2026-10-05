//! Windows: Media Foundation's sink writer with Microsoft's H.264 and AAC
//! encoders; it writes the MP4 itself.
//!
//! The software H.264 encoder is used on purpose: it starts in milliseconds
//! and outpaces the GPU encoders' Media Foundation wrappers, which take most
//! of a second to start (the start of the recording would be lost).
//!
//! HDR recordings are HEVC Main 10 (HDR10), which only the GPU encoders
//! (NVIDIA, AMD, Intel) do; they're given P010 frames (see `hdr::to_p010`).
//! AV1 also needs a GPU encoder (NVIDIA RTX 40, AMD RX 7000, Intel Arc and
//! newer).

use std::path::{Path, PathBuf};

use windows::Win32::Media::MediaFoundation::*;
use windows::core::{GUID, HSTRING};

use super::{AudioFormat, Encoder, HdrInfo, VideoFormat, faststart, rgba_to_nv12, video_bitrate};
use crate::mf::{Session, attributes};

/// Media Foundation time is in 100 ns units.
const TICKS: u64 = 10_000_000;

/// AAC at 160 kbit/s (Microsoft's encoder takes 96, 128, 160 or 192).
const AAC_BYTES_PER_SECOND: u32 = 20_000;

pub struct MediaFoundation {
    // Declared (and so dropped) before `_session`, which shuts MF down.
    // `None` once finished, so the file is closed.
    writer: Option<Writer>,
    _session: Session,
    out: PathBuf,
    video: VideoFormat,
    audio: Option<AudioFormat>,
    codec: Codec,
    frames: u64,
    samples: u64,
}

/// What the video is encoded as.
#[derive(Clone, Copy)]
enum Codec {
    H264,
    Av1,
    /// HEVC Main 10 (HDR10) from P010 frames, rather than from RGBA.
    Hdr(HdrInfo),
}

impl Codec {
    fn hdr(self) -> Option<HdrInfo> {
        match self {
            Codec::Hdr(info) => Some(info),
            _ => None,
        }
    }
}

struct Writer {
    sink: IMFSinkWriter,
    video_stream: u32,
    audio_stream: Option<u32>,
}

impl MediaFoundation {
    pub fn open(out: &Path, video: VideoFormat, audio: Option<AudioFormat>) -> Result<Self, String> {
        Self::open_as(out, video, audio, Codec::H264)
    }

    /// An AV1 recording, from a GPU encoder.
    pub fn open_av1(out: &Path, video: VideoFormat, audio: Option<AudioFormat>) -> Result<Self, String> {
        Self::open_as(out, video, audio, Codec::Av1)
    }

    /// An HDR10 recording; takes P010 frames.
    pub fn open_hdr(out: &Path, video: VideoFormat, audio: Option<AudioFormat>, hdr: HdrInfo) -> Result<Self, String> {
        Self::open_as(out, video, audio, Codec::Hdr(hdr))
    }

    fn open_as(out: &Path, video: VideoFormat, audio: Option<AudioFormat>, codec: Codec) -> Result<Self, String> {
        let session = Session::start()?;
        // SAFETY: COM calls on objects created here. Variable bit rate (a
        // still screen takes little space) is a request the encoder may
        // turn down; then it's left to choose.
        let writer = unsafe {
            Writer::create(out, video, audio, codec, true).or_else(|_| Writer::create(out, video, audio, codec, false))
        }
        .map_err(|e| match codec {
            Codec::Hdr(_) => format!("couldn't set up an HDR (HEVC 10-bit) encoder: {}", e.message()),
            Codec::Av1 => format!(
                "couldn't set up an AV1 encoder (it takes an NVIDIA RTX 40, AMD RX 7000 or Intel Arc graphics card, or newer): {}",
                e.message()
            ),
            Codec::H264 => format!("couldn't set up the video encoder: {}", e.message()),
        })?;
        Ok(Self {
            writer: Some(writer),
            _session: session,
            out: out.to_owned(),
            video,
            audio,
            codec,
            frames: 0,
            samples: 0,
        })
    }

    fn writer(&self) -> Result<&Writer, String> {
        self.writer.as_ref().ok_or_else(|| "the video file is finished".into())
    }
}

impl Encoder for MediaFoundation {
    fn video(&mut self, rgba: &[u8]) -> Result<(), String> {
        let (w, h, fps) = (
            self.video.width as usize,
            self.video.height as usize,
            self.video.fps.max(1) as u64,
        );
        let time = self.frames * TICKS / fps;
        let duration = (self.frames + 1) * TICKS / fps - time;
        let writer = self.writer()?;
        let hdr = self.codec.hdr().is_some();
        // NV12 has a byte per sample, P010 two.
        let len = w * h * 3 / 2 * if hdr { 2 } else { 1 };
        if hdr && rgba.len() != len {
            return Err("the HDR frame has the wrong size".into());
        }
        // SAFETY: the sample's buffer is `len` bytes, as filled in.
        unsafe {
            writer.write(writer.video_stream, len, time, duration, |buf| {
                if hdr {
                    buf.copy_from_slice(rgba)
                } else {
                    rgba_to_nv12(rgba, w, h, buf)
                }
            })
        }
        .map_err(|e| format!("the video encoder failed: {}", e.message()))?;
        self.frames += 1;
        Ok(())
    }

    fn audio(&mut self, samples: &[f32]) -> Result<(), String> {
        let (Some(stream), Some(format)) = (self.writer()?.audio_stream, self.audio) else {
            return Ok(());
        };
        let channels = format.channels.max(1) as usize;
        let frames = (samples.len() / channels) as u64;
        if frames == 0 {
            return Ok(());
        }
        let rate = format.sample_rate as u64;
        let time = self.samples * TICKS / rate;
        let duration = (self.samples + frames) * TICKS / rate - time;
        let samples = &samples[..frames as usize * channels];
        // SAFETY: the buffer holds two bytes per sample.
        unsafe {
            self.writer()?.write(stream, samples.len() * 2, time, duration, |buf| {
                for (dst, s) in buf.chunks_exact_mut(2).zip(samples) {
                    let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
                    dst.copy_from_slice(&v.to_le_bytes());
                }
            })
        }
        .map_err(|e| format!("the audio encoder failed: {}", e.message()))?;
        self.samples += frames;
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<Option<String>, String> {
        // An audio track without a single sample makes Finalize fail.
        if self.writer()?.audio_stream.is_some() && self.samples == 0 {
            let silence = vec![0.0; 2 * 1024];
            self.audio(&silence)?;
        }
        // SAFETY: finishes the file the writer was created for.
        unsafe { self.writer()?.sink.Finalize() }
            .map_err(|e| format!("couldn't finish the video file: {}", e.message()))?;
        // Closes the file. Media Foundation can put the index first itself
        // (MF_MPEG4SINK_MOOV_BEFORE_MDAT), but the files come out broken.
        self.writer = None;
        if let Err(e) = faststart::apply(&self.out) {
            eprintln!("couldn't move the index of {}: {e}", self.out.display());
        }
        Ok(None)
    }
}

impl Writer {
    /// A sink writer for `out` with the streams set up and writing begun.
    unsafe fn create(
        out: &Path,
        video: VideoFormat,
        audio: Option<AudioFormat>,
        codec: Codec,
        vbr: bool,
    ) -> windows::core::Result<Self> {
        unsafe {
            let hdr = codec.hdr();
            let writer_attributes = attributes(2)?;
            // Only GPU encoders do 10-bit HEVC and AV1.
            let hardware = !matches!(codec, Codec::H264);
            writer_attributes.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, hardware as u32)?;
            writer_attributes.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
            let sink = MFCreateSinkWriterFromURL(
                &HSTRING::from(out),
                None::<&IMFByteStream>,
                &writer_attributes,
            )?;

            let (encoded, input) = match codec {
                Codec::Hdr(_) => (&MFVideoFormat_HEVC, &MFVideoFormat_P010),
                Codec::Av1 => (&MFVideoFormat_AV1, &MFVideoFormat_NV12),
                Codec::H264 => (&MFVideoFormat_H264, &MFVideoFormat_NV12),
            };
            let compressed = video_type(video, encoded, hdr)?;
            // 10 bits and a wider range of brightness take more.
            let bitrate = video_bitrate(video) * if hdr.is_some() { 3 } else { 2 } / 2;
            compressed.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
            match codec {
                Codec::Hdr(info) => {
                    compressed.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH265VProfile_Main_420_10.0 as u32)?;
                    // Mastering display: the screen it was recorded on.
                    let peak = info.peak_nits.round() as u32;
                    compressed.SetUINT32(&MF_MT_MAX_MASTERING_LUMINANCE, peak)?;
                    compressed.SetUINT32(&MF_MT_MIN_MASTERING_LUMINANCE, 50)?; // 0.005 nits
                    compressed.SetUINT32(&MF_MT_MAX_LUMINANCE_LEVEL, peak)?;
                    compressed.SetUINT32(&MF_MT_MAX_FRAME_AVERAGE_LUMINANCE_LEVEL, info.sdr_white_nits.round() as u32)?;
                }
                Codec::Av1 => compressed.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncAV1VProfile_Main_420_8.0 as u32)?,
                Codec::H264 => compressed.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?,
            }
            let video_stream = sink.AddStream(&compressed)?;
            let frames = video_type(video, input, hdr)?;
            if hdr.is_none() {
                frames.SetUINT32(&MF_MT_DEFAULT_STRIDE, video.width)?;
            }
            let bitrate_param = bitrate;
            let params = if vbr {
                let p = attributes(2)?;
                p.SetUINT32(
                    &CODECAPI_AVEncCommonRateControlMode,
                    eAVEncCommonRateControlMode_UnconstrainedVBR.0 as u32,
                )?;
                p.SetUINT32(&CODECAPI_AVEncCommonMeanBitRate, bitrate_param)?;
                Some(p)
            } else {
                None
            };
            sink.SetInputMediaType(video_stream, &frames, params.as_ref())?;

            let audio_stream = match audio {
                Some(a) => {
                    let aac = audio_type(a, &MFAudioFormat_AAC)?;
                    aac.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AAC_BYTES_PER_SECOND)?;
                    let stream = sink.AddStream(&aac)?;
                    let pcm = audio_type(a, &MFAudioFormat_PCM)?;
                    let block = a.channels as u32 * 2;
                    pcm.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, block)?;
                    pcm.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, block * a.sample_rate)?;
                    sink.SetInputMediaType(stream, &pcm, None::<&IMFAttributes>)?;
                    Some(stream)
                }
                None => None,
            };
            sink.BeginWriting()?;
            Ok(Self {
                sink,
                video_stream,
                audio_stream,
            })
        }
    }

    /// Writes a sample of `len` bytes, filled in by `fill`, to `stream`.
    unsafe fn write(
        &self,
        stream: u32,
        len: usize,
        time: u64,
        duration: u64,
        fill: impl FnOnce(&mut [u8]),
    ) -> windows::core::Result<()> {
        unsafe {
            let buffer = MFCreateMemoryBuffer(len as u32)?;
            let mut data = std::ptr::null_mut();
            buffer.Lock(&mut data, None, None)?;
            fill(std::slice::from_raw_parts_mut(data, len));
            buffer.Unlock()?;
            buffer.SetCurrentLength(len as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time as i64)?;
            sample.SetSampleDuration(duration as i64)?;
            self.sink.WriteSample(stream, &sample)
        }
    }
}

/// A video media type: size, frame rate and colour: BT.709 limited range
/// (what `rgba_to_nv12` produces), or for HDR, BT.2020 PQ (HDR10).
unsafe fn video_type(v: VideoFormat, subtype: &GUID, hdr: Option<HdrInfo>) -> windows::core::Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, (v.width as u64) << 32 | v.height as u64)?;
        t.SetUINT64(&MF_MT_FRAME_RATE, (v.fps.max(1) as u64) << 32 | 1)?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, 1 << 32 | 1)?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        let (matrix, primaries, transfer) = match hdr {
            Some(_) => (MFVideoTransferMatrix_BT2020_10, MFVideoPrimaries_BT2020, MFVideoTransFunc_2084),
            None => (MFVideoTransferMatrix_BT709, MFVideoPrimaries_BT709, MFVideoTransFunc_709),
        };
        t.SetUINT32(&MF_MT_YUV_MATRIX, matrix.0 as u32)?;
        t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
        t.SetUINT32(&MF_MT_VIDEO_PRIMARIES, primaries.0 as u32)?;
        t.SetUINT32(&MF_MT_TRANSFER_FUNCTION, transfer.0 as u32)?;
        Ok(t)
    }
}

/// A 16-bit audio media type at `a`'s rate and channel count.
unsafe fn audio_type(a: AudioFormat, subtype: &GUID) -> windows::core::Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, a.sample_rate)?;
        t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, a.channels as u32)?;
        Ok(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records three seconds of moving frames in AV1 to
    /// `%TEMP%/snapr-av1-test.mp4`: `cargo test av1_round_trip -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn av1_round_trip() {
        let out = std::env::temp_dir().join("snapr-av1-test.mp4");
        let (w, h, fps) = (1920u32, 1080u32, 60u32);
        let video = VideoFormat { width: w, height: h, fps };
        let audio = AudioFormat { sample_rate: 48_000, channels: 2 };
        let began = std::time::Instant::now();
        let mut enc = MediaFoundation::open_av1(&out, video, Some(audio)).unwrap();
        println!("opened in {:?}", began.elapsed());
        let began = std::time::Instant::now();
        for n in 0..3 * fps {
            let frame: Vec<u8> = (0..w * h)
                .flat_map(|i| [((i % w + n * 8) % 256) as u8, (i / w % 256) as u8, 120, 255])
                .collect();
            enc.video(&frame).unwrap();
            enc.audio(&vec![0.0; 800 * 2]).unwrap();
        }
        Box::new(enc).finish().unwrap();
        println!(
            "encoded in {:?}: {} ({} bytes)",
            began.elapsed(),
            out.display(),
            std::fs::metadata(&out).unwrap().len()
        );

        // The gallery and player decode it too (with the AV1 Video Extension).
        let info = crate::decode::probe(&out, "").expect("probe");
        let mut pictures = 0;
        crate::decode::pictures(&out, "", info, 0.0, (640, 360), &mut |_| {
            pictures += 1;
            true
        })
        .unwrap();
        println!("decoded {pictures} pictures: {info:?}");
        assert_eq!(pictures, 3 * fps as usize);
    }
}
