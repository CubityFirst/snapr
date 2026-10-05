//! Windows: the VP9 encoder from the VP9 Video Extensions (installed with
//! Windows 10/11), driven directly: Media Foundation's file writers don't
//! do WebM, so `webm` writes the file.

use windows::Win32::Media::MediaFoundation::*;
use windows::core::Interface;

use super::{VideoFormat, video_bitrate};

/// Media Foundation time is in 100 ns units.
pub const TICKS: u64 = 10_000_000;

/// A key frame at least this often (seconds), so seeking stays quick.
const KEY_FRAME_INTERVAL: u32 = 2;

pub struct Vp9 {
    mft: IMFTransform,
    /// The encoder hands out its own output samples.
    provides_samples: bool,
    output_size: u32,
}

/// An encoded frame.
pub struct Packet {
    pub data: Vec<u8>,
    /// 100 ns units.
    pub time: u64,
    pub key: bool,
}

impl Vp9 {
    /// The VP9 encoder, if Windows has one, set up for NV12 frames of
    /// `video`'s size. Media Foundation must be started on this thread.
    pub fn open(video: VideoFormat) -> Result<Self, String> {
        // SAFETY: Media Foundation calls on objects created here.
        unsafe { Self::create(video) }.map_err(|e| match e {
            Some(e) => format!("couldn't set up the VP9 encoder: {}", e.message()),
            None => "Windows has no VP9 encoder (it comes with the VP9 Video Extensions from the Microsoft Store)".into(),
        })
    }

    unsafe fn create(video: VideoFormat) -> Result<Self, Option<windows::core::Error>> {
        unsafe {
            let info = MFT_REGISTER_TYPE_INFO {
                guidMajorType: MFMediaType_Video,
                guidSubtype: MFVideoFormat_VP90,
            };
            let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut count = 0;
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_LOCALMFT | MFT_ENUM_FLAG_SORTANDFILTER,
                None,
                Some(&info),
                &mut list,
                &mut count,
            )
            .map_err(Some)?;
            let activates: Vec<IMFActivate> = std::slice::from_raw_parts_mut(list, count as usize)
                .iter_mut()
                .filter_map(Option::take)
                .collect();
            windows::Win32::System::Com::CoTaskMemFree(Some(list.cast()));
            let activate = activates.first().ok_or(None)?;
            let mft: IMFTransform = activate.ActivateObject().map_err(Some)?;

            let vp9 = MFCreateMediaType().map_err(Some)?;
            vp9.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(Some)?;
            vp9.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_VP90).map_err(Some)?;
            set_video(&vp9, video).map_err(Some)?;
            vp9.SetUINT32(&MF_MT_AVG_BITRATE, video_bitrate(video)).map_err(Some)?;
            mft.SetOutputType(0, &vp9, 0).map_err(Some)?;
            let nv12 = MFCreateMediaType().map_err(Some)?;
            nv12.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(Some)?;
            nv12.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12).map_err(Some)?;
            set_video(&nv12, video).map_err(Some)?;
            mft.SetInputType(0, &nv12, 0).map_err(Some)?;
            if let Ok(api) = mft.cast::<ICodecAPI>() {
                let gop = video.fps.max(1) * KEY_FRAME_INTERVAL;
                let _ = api.SetValue(&CODECAPI_AVEncMPVGOPSize, &variant_u32(gop));
            }

            let stream = mft.GetOutputStreamInfo(0).map_err(Some)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).map_err(Some)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).map_err(Some)?;
            Ok(Self {
                mft,
                provides_samples: stream.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0,
                output_size: stream.cbSize.max(video.width * video.height * 3 / 2),
            })
        }
    }

    /// Encodes an NV12 frame shown at `time` (100 ns) for `duration`,
    /// returning whatever packets are ready.
    pub fn encode(&mut self, nv12: &[u8], time: u64, duration: u64) -> Result<Vec<Packet>, String> {
        // SAFETY: as in `open`.
        unsafe {
            let buffer = MFCreateMemoryBuffer(nv12.len() as u32).map_err(err)?;
            let mut data = std::ptr::null_mut();
            buffer.Lock(&mut data, None, None).map_err(err)?;
            std::ptr::copy_nonoverlapping(nv12.as_ptr(), data, nv12.len());
            buffer.Unlock().map_err(err)?;
            buffer.SetCurrentLength(nv12.len() as u32).map_err(err)?;
            let sample = MFCreateSample().map_err(err)?;
            sample.AddBuffer(&buffer).map_err(err)?;
            sample.SetSampleTime(time as i64).map_err(err)?;
            sample.SetSampleDuration(duration as i64).map_err(err)?;
            let mut packets = Vec::new();
            loop {
                match self.mft.ProcessInput(0, &sample, 0) {
                    Ok(()) => break,
                    // Full: take what's done first.
                    Err(e) if e.code() == MF_E_NOTACCEPTING => {
                        let before = packets.len();
                        self.take(&mut packets)?;
                        if packets.len() == before {
                            return Err("the VP9 encoder stopped taking frames".into());
                        }
                    }
                    Err(e) => return Err(err(e)),
                }
            }
            self.take(&mut packets)?;
            Ok(packets)
        }
    }

    /// The packets still inside the encoder, at the end.
    pub fn finish(&mut self) -> Result<Vec<Packet>, String> {
        let mut packets = Vec::new();
        // SAFETY: as in `open`.
        unsafe {
            self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0).map_err(err)?;
            self.take(&mut packets)?;
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
        }
        Ok(packets)
    }

    /// Collects encoded packets until the encoder wants more input.
    unsafe fn take(&mut self, packets: &mut Vec<Packet>) -> Result<(), String> {
        loop {
            // SAFETY: the output buffer and sample are released below.
            unsafe {
                let sample = if self.provides_samples {
                    None
                } else {
                    let s = MFCreateSample().map_err(err)?;
                    s.AddBuffer(&MFCreateMemoryBuffer(self.output_size).map_err(err)?)
                        .map_err(err)?;
                    Some(s)
                };
                let mut output = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: std::mem::ManuallyDrop::new(sample),
                    dwStatus: 0,
                    pEvents: std::mem::ManuallyDrop::new(None),
                }];
                let mut status = 0;
                let result = self.mft.ProcessOutput(0, &mut output, &mut status);
                let sample = std::mem::ManuallyDrop::take(&mut output[0].pSample);
                drop(std::mem::ManuallyDrop::take(&mut output[0].pEvents));
                match result {
                    Ok(()) => {}
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        // The encoder settled on its output type; keep it.
                        let t = self.mft.GetOutputAvailableType(0, 0).map_err(err)?;
                        self.mft.SetOutputType(0, &t, 0).map_err(err)?;
                        continue;
                    }
                    Err(e) => return Err(err(e)),
                }
                let Some(sample) = sample else {
                    continue;
                };
                let buffer = sample.ConvertToContiguousBuffer().map_err(err)?;
                let mut data = std::ptr::null_mut();
                let mut len = 0;
                buffer.Lock(&mut data, None, Some(&mut len)).map_err(err)?;
                let bytes = std::slice::from_raw_parts(data, len as usize).to_vec();
                buffer.Unlock().map_err(err)?;
                if bytes.is_empty() {
                    continue;
                }
                packets.push(Packet {
                    data: bytes,
                    time: sample.GetSampleTime().unwrap_or(0).max(0) as u64,
                    key: sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0,
                });
            }
        }
    }
}

fn err(e: windows::core::Error) -> String {
    format!("the VP9 encoder failed: {}", e.message())
}

unsafe fn set_video(t: &IMFMediaType, v: VideoFormat) -> windows::core::Result<()> {
    unsafe {
        t.SetUINT64(&MF_MT_FRAME_SIZE, (v.width as u64) << 32 | v.height as u64)?;
        t.SetUINT64(&MF_MT_FRAME_RATE, (v.fps.max(1) as u64) << 32 | 1)?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, 1 << 32 | 1)?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
    }
}

fn variant_u32(v: u32) -> windows::Win32::System::Variant::VARIANT {
    let mut variant = windows::Win32::System::Variant::VARIANT::default();
    // SAFETY: a VARIANT holding an unsigned 32-bit integer.
    unsafe {
        let value = &mut *variant.Anonymous.Anonymous;
        value.vt = windows::Win32::System::Variant::VT_UI4;
        value.Anonymous.ulVal = v;
    }
    variant
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encodes three seconds of moving frames: `cargo test vp9_encodes -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn vp9_encodes() {
        let _session = crate::mf::Session::start().unwrap();
        let video = VideoFormat {
            width: 640,
            height: 360,
            fps: 30,
        };
        let mut enc = Vp9::open(video).unwrap();
        println!("provides samples: {}", enc.provides_samples);
        let (w, h) = (640usize, 360usize);
        let mut packets = Vec::new();
        for n in 0..90u64 {
            let rgba: Vec<u8> = (0..w * h)
                .flat_map(|i| [((i % w) as u64 * 2 + n * 4) as u8, (i / w) as u8, 100, 255])
                .collect();
            let mut nv12 = vec![0; w * h * 3 / 2];
            super::super::rgba_to_nv12(&rgba, w, h, &mut nv12);
            let t = n * TICKS / 30;
            packets.extend(enc.encode(&nv12, t, TICKS / 30).unwrap());
        }
        packets.extend(enc.finish().unwrap());
        let keys: Vec<_> = packets.iter().enumerate().filter(|(_, p)| p.key).map(|(i, _)| i).collect();
        println!(
            "{} packets, {} bytes, key frames at {keys:?}, times {:?}",
            packets.len(),
            packets.iter().map(|p| p.data.len()).sum::<usize>(),
            packets.iter().take(4).map(|p| p.time).collect::<Vec<_>>()
        );
        assert_eq!(packets.len(), 90);
        assert!(packets[0].key);
    }
}
