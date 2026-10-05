//! Windows: Media Foundation's source reader, which decodes whatever Windows
//! has codecs for (H.264, HEVC, VP8/VP9, AV1, ...) into frames and float
//! sound. Frames come as NV12 (what decoders produce) and are converted
//! here; RGB32 through Media Foundation's converter is the fallback.

use std::path::Path;

use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantToUInt64};
use windows::Win32::System::Variant::VT_I8;
use windows::core::{GUID, HSTRING};

use super::{Info, Layout, MAX_FPS, Matrix, Picture, Yuv, picture_from, picture_from_nv12};
use crate::mf::{Session, attributes};

/// Media Foundation time is in 100 ns units.
const TICKS: f64 = 10_000_000.0;
const VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
const AUDIO: u32 = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;

type MfResult<T> = windows::core::Result<T>;

pub fn probe(path: &Path, _ffmpeg: &str) -> Option<Info> {
    let _session = Session::start().ok()?;
    // SAFETY: Media Foundation calls on objects created here, released
    // before the session.
    unsafe {
        let reader = reader(path, VIDEO).ok()?;
        let native = reader.GetNativeMediaType(VIDEO, 0).ok()?;
        let fps = native
            .GetUINT64(&MF_MT_FRAME_RATE)
            .ok()
            .map(|r| ((r >> 32) as f64, (r & 0xffff_ffff) as f64))
            .filter(|&(n, d)| n > 0.0 && d > 0.0)
            .map_or(30.0, |(n, d)| n / d);
        let duration = reader
            .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
            .ok()
            .and_then(|v| PropVariantToUInt64(&v).ok())
            .map(|t| t as f64 / TICKS);
        Some(Info {
            duration,
            fps: fps.min(MAX_FPS),
        })
    }
}

pub fn pictures(
    path: &Path,
    _ffmpeg: &str,
    info: Info,
    from: f64,
    max: (u32, u32),
    each: &mut dyn FnMut(Picture) -> bool,
) -> Result<(), String> {
    let _session = Session::start()?;
    // SAFETY: as in `probe`.
    unsafe { decode_pictures(path, info, from, max, each) }.map_err(|e| {
        // The codec is known but not this variant of it, e.g. 4:4:4 VP9.
        if e.code() == MF_E_TRANSFORM_NOT_POSSIBLE_FOR_CURRENT_OUTPUT_MEDIATYPE {
            "Windows can't decode this kind of video".to_string()
        } else {
            format!("couldn't decode the video: {}", e.message())
        }
    })
}

/// How the reader's frames are laid out.
#[derive(Debug, Clone, Copy)]
enum Frames {
    /// The Y plane as `Layout`, the U/V plane from `uv_top`.
    Nv12 { y: Layout, uv_top: usize, yuv: Yuv },
    /// Blue, green, red, unused.
    Rgb32(Layout),
}

unsafe fn decode_pictures(
    path: &Path,
    info: Info,
    from: f64,
    max: (u32, u32),
    each: &mut dyn FnMut(Picture) -> bool,
) -> MfResult<()> {
    unsafe {
        let reader = reader(path, VIDEO)?;
        let output = |subtype: &GUID| -> MfResult<()> {
            let t = MFCreateMediaType()?;
            t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
            reader.SetCurrentMediaType(VIDEO, None, &t)
        };
        output(&MFVideoFormat_NV12).or_else(|_| output(&MFVideoFormat_RGB32))?;
        seek(&reader, from)?;
        // The seek lands on the key frame before `from`; the frames from
        // there are decoded but not shown.
        let first = from - 0.5 / info.fps;
        while let Some((time, sample, _)) = read(&reader, VIDEO)? {
            let Some(sample) = sample else {
                continue;
            };
            if time < first {
                continue;
            }
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut data = std::ptr::null_mut();
            let mut len = 0;
            buffer.Lock(&mut data, None, Some(&mut len))?;
            let data = std::slice::from_raw_parts(data, len as usize);
            // Read the layout per frame: it can change mid-stream, and the
            // padding depends on the buffer.
            let image = frames(&reader, data.len()).ok().and_then(|f| match f {
                Frames::Nv12 { y, uv_top, yuv } => picture_from_nv12(data, y, uv_top, yuv, max),
                Frames::Rgb32(layout) => picture_from(data, layout, [2, 1, 0], max),
            });
            buffer.Unlock()?;
            let image = image.ok_or_else(|| windows::core::Error::from(MF_E_INVALID_FORMAT))?;
            if !each(Picture { time, image }) {
                break;
            }
        }
        Ok(())
    }
}

pub fn sound(
    path: &Path,
    _ffmpeg: &str,
    from: f64,
    rate: u32,
    channels: u16,
    each: &mut dyn FnMut(Vec<f32>) -> bool,
) -> Result<bool, String> {
    let _session = Session::start()?;
    // SAFETY: as in `probe`.
    unsafe {
        // Selecting the audio stream fails when there is none.
        let Ok(reader) = reader(path, AUDIO) else {
            return Ok(false);
        };
        decode_sound(&reader, from, rate, channels, each)
            .map(|()| true)
            .map_err(|e| format!("couldn't decode the sound: {}", e.message()))
    }
}

unsafe fn decode_sound(
    reader: &IMFSourceReader,
    from: f64,
    rate: u32,
    channels: u16,
    each: &mut dyn FnMut(Vec<f32>) -> bool,
) -> MfResult<()> {
    unsafe {
        // The reader resamples and remixes to what the output device takes.
        let float = MFCreateMediaType()?;
        float.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        float.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float)?;
        float.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate)?;
        float.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels as u32)?;
        float.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 32)?;
        float.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, channels as u32 * 4)?;
        float.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, channels as u32 * 4 * rate)?;
        reader.SetCurrentMediaType(AUDIO, None, &float)?;
        seek(reader, from)?;
        while let Some((time, sample, _)) = read(reader, AUDIO)? {
            let Some(sample) = sample else {
                continue;
            };
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut data = std::ptr::null_mut();
            let mut len = 0;
            buffer.Lock(&mut data, None, Some(&mut len))?;
            let bytes = std::slice::from_raw_parts(data, len as usize);
            let mut samples: Vec<f32> = bytes
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            buffer.Unlock()?;
            // Leave out what comes before `from` (the seek lands earlier).
            if time < from {
                let skip = ((from - time) * rate as f64) as usize * channels as usize;
                samples.drain(..skip.min(samples.len()));
            }
            if !samples.is_empty() && !each(samples) {
                break;
            }
        }
        Ok(())
    }
}

/// A source reader for `path` with only `stream` selected.
unsafe fn reader(path: &Path, stream: u32) -> MfResult<IMFSourceReader> {
    unsafe {
        let attributes = attributes(1)?;
        // Lets it convert to RGB where a decoder can't give NV12.
        attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
        let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path), &attributes)?;
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
        reader.SetStreamSelection(stream, true)?;
        Ok(reader)
    }
}

/// Moves the reader to `to` seconds (in practice, the key frame before it).
unsafe fn seek(reader: &IMFSourceReader, to: f64) -> MfResult<()> {
    if to <= 0.0 {
        return Ok(());
    }
    let mut position = PROPVARIANT::default();
    // SAFETY: a PROPVARIANT holding a 64-bit integer.
    unsafe {
        let value = &mut *position.Anonymous.Anonymous;
        value.vt = VT_I8;
        value.Anonymous.hVal = (to * TICKS) as i64;
        reader.SetCurrentPosition(&GUID::zeroed(), &position)
    }
}

/// The next sample of `stream` with its time in seconds and the reader's
/// flags; `None` at the end. The sample can be missing (a gap).
unsafe fn read(reader: &IMFSourceReader, stream: u32) -> MfResult<Option<(f64, Option<IMFSample>, u32)>> {
    let (mut flags, mut time, mut sample) = (0u32, 0i64, None);
    unsafe { reader.ReadSample(stream, 0, None, Some(&mut flags), Some(&mut time), Some(&mut sample))? };
    if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
        return Ok(None);
    }
    Ok(Some((time as f64 / TICKS, sample, flags)))
}

/// Where the picture is in a `len`-byte frame from the reader: its format,
/// row order and padding, and the visible part (decoders round sizes up to
/// whole macroblocks).
unsafe fn frames(reader: &IMFSourceReader, len: usize) -> MfResult<Frames> {
    unsafe {
        let t = reader.GetCurrentMediaType(VIDEO)?;
        let size = t.GetUINT64(&MF_MT_FRAME_SIZE)?;
        let (w, h) = ((size >> 32) as u32, (size & 0xffff_ffff) as u32);
        let nv12 = t.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12;
        let (pixel, rows) = if nv12 { (1, h as usize * 3 / 2) } else { (4, h as usize) };
        // Buffers can be padded beyond the default stride; the length says
        // by how much.
        let stride = match t.GetUINT32(&MF_MT_DEFAULT_STRIDE) {
            Ok(s) if (s as i32).unsigned_abs() as usize * rows == len => s as i32 as isize,
            Ok(s) if (s as i32) < 0 => -((len / rows) as isize),
            // RGB frames are bottom-up unless the stride says otherwise.
            Err(_) if !nv12 => -((len / rows) as isize),
            _ => (len / rows) as isize,
        };
        if stride.unsigned_abs() < w as usize * pixel {
            return Err(MF_E_INVALID_FORMAT.into());
        }
        let top = if stride < 0 { (h as isize - 1) * -stride } else { 0 } as usize;
        // MFVideoArea: x and y as 16.16 fixed point, then width and height.
        let mut area = [0u8; 16];
        let (mut x, mut y, mut vw, mut vh) = (0, 0, w, h);
        if t.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, &mut area, None).is_ok() {
            let int = |i: usize| i32::from_le_bytes(area[i..i + 4].try_into().unwrap());
            let (ax, ay) = ((int(0) >> 16).max(0) as u32, (int(4) >> 16).max(0) as u32);
            let (aw, ah) = (int(8).max(0) as u32, int(12).max(0) as u32);
            if aw > 0 && ah > 0 && ax + aw <= w && ay + ah <= h {
                (x, y, vw, vh) = (ax, ay, aw, ah);
            }
        }
        if !nv12 {
            return Ok(Frames::Rgb32(Layout {
                w: vw,
                h: vh,
                top: (top as isize + y as isize * stride) as usize + x as usize * 4,
                stride,
            }));
        }
        // Even offsets keep the chroma lined up with the luma.
        let (x, y) = (x & !1, y & !1);
        let full = Layout {
            w: vw,
            h: vh,
            top: y as usize * stride as usize + x as usize,
            stride,
        };
        // Colour tags can be on the decoded type or only on the file's.
        let native = reader.GetNativeMediaType(VIDEO, 0).ok();
        let tag = |key: &GUID| t.GetUINT32(key).ok().or_else(|| native.as_ref()?.GetUINT32(key).ok());
        let matrix = match tag(&MF_MT_YUV_MATRIX) {
            Some(m) if m == MFVideoTransferMatrix_BT2020_10.0 as u32 || m == MFVideoTransferMatrix_BT2020_12.0 as u32 => {
                Matrix::Bt2020
            }
            Some(m) if m == MFVideoTransferMatrix_BT709.0 as u32 => Matrix::Bt709,
            Some(m) if m == MFVideoTransferMatrix_BT601.0 as u32 => Matrix::Bt601,
            // Unmarked: HD is usually BT.709, SD BT.601.
            _ if h >= 720 => Matrix::Bt709,
            _ => Matrix::Bt601,
        };
        let yuv = Yuv {
            matrix,
            full_range: tag(&MF_MT_VIDEO_NOMINAL_RANGE) == Some(MFNominalRange_0_255.0 as u32),
            pq: tag(&MF_MT_TRANSFER_FUNCTION) == Some(MFVideoTransFunc_2084.0 as u32),
        };
        Ok(Frames::Nv12 {
            y: full,
            uv_top: h as usize * stride as usize + y as usize / 2 * stride as usize + x as usize,
            yuv,
        })
    }
}
