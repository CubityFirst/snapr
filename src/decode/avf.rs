//! macOS: AVFoundation's asset reader decodes whatever the system has codecs
//! for (H.264, HEVC, ...) into BGRA frames and float sound.

use std::path::Path;
use std::ptr::NonNull;

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2_av_foundation::{
    AVAssetReader, AVAssetReaderTrackOutput, AVAssetTrack, AVMediaType, AVMediaTypeAudio,
    AVMediaTypeVideo, AVURLAsset,
};
use objc2_core_audio_types::kAudioFormatLinearPCM;
use objc2_core_foundation::CFString;
use objc2_core_media::{CMTime, CMTimeRange, kCMTimePositiveInfinity};
use objc2_core_video::{
    CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
    CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferUnlockBaseAddress, kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_32BGRA,
};
use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};

use super::{Info, Layout, MAX_FPS, Picture, picture_from};

pub fn probe(path: &Path, _ffmpeg: &str) -> Option<Info> {
    autoreleasepool(|_| unsafe {
        let asset = asset(path)?;
        let track = track(&asset, AVMediaTypeVideo?)?;
        let fps = track.nominalFrameRate() as f64;
        #[allow(deprecated)]
        let duration = asset.duration().seconds();
        Some(Info {
            duration: duration.is_finite().then_some(duration),
            fps: if fps > 0.0 { fps.min(MAX_FPS) } else { 30.0 },
        })
    })
}

pub fn pictures(
    path: &Path,
    _ffmpeg: &str,
    info: Info,
    from: f64,
    max: (u32, u32),
    each: &mut dyn FnMut(Picture) -> bool,
) -> Result<(), String> {
    unsafe {
        let settings = NSDictionary::<NSString, AnyObject>::from_slices(
            &[cf_key(kCVPixelBufferPixelFormatTypeKey)],
            &[NSNumber::new_u32(kCVPixelFormatType_32BGRA).as_ref()],
        );
        let video = AVMediaTypeVideo.ok_or("no video media type")?;
        let (reader, output) = open(path, video, &settings, from)?;
        // The reader starts at the key frame before `from`.
        let first = from - 0.5 / info.fps;
        loop {
            let picture = autoreleasepool(|_| -> Result<Option<Option<Picture>>, String> {
                let Some(sample) = output.copyNextSampleBuffer() else {
                    return Ok(None);
                };
                let time = sample.presentation_time_stamp().seconds();
                if time < first {
                    return Ok(Some(None));
                }
                let Some(buffer) = sample.image_buffer() else {
                    return Ok(Some(None));
                };
                CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags::ReadOnly);
                let (w, h) = (CVPixelBufferGetWidth(&buffer), CVPixelBufferGetHeight(&buffer));
                let stride = CVPixelBufferGetBytesPerRow(&buffer);
                let base = CVPixelBufferGetBaseAddress(&buffer).cast::<u8>();
                let image = (!base.is_null())
                    .then(|| {
                        let data = std::slice::from_raw_parts(base, stride * h);
                        let layout = Layout {
                            w: w as u32,
                            h: h as u32,
                            top: 0,
                            stride: stride as isize,
                        };
                        picture_from(data, layout, [2, 1, 0], max)
                    })
                    .flatten();
                CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags::ReadOnly);
                let image = image.ok_or("a frame came out unreadable")?;
                Ok(Some(Some(Picture { time, image })))
            })?;
            match picture {
                None => break,
                Some(None) => continue,
                Some(Some(p)) => {
                    if !each(p) {
                        reader.cancelReading();
                        break;
                    }
                }
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
    unsafe {
        // AVFAudio's key constants, by value. The reader resamples and
        // remixes to what the output device takes.
        let key = |k: &str| NSString::from_str(k);
        let keys = [
            key("AVFormatIDKey"),
            key("AVSampleRateKey"),
            key("AVNumberOfChannelsKey"),
            key("AVLinearPCMBitDepthKey"),
            key("AVLinearPCMIsFloatKey"),
            key("AVLinearPCMIsBigEndianKey"),
            key("AVLinearPCMIsNonInterleaved"),
        ];
        let values = [
            NSNumber::new_u32(kAudioFormatLinearPCM),
            NSNumber::new_f64(rate as f64),
            NSNumber::new_u32(channels as u32),
            NSNumber::new_u32(32),
            NSNumber::new_bool(true),
            NSNumber::new_bool(false),
            NSNumber::new_bool(false),
        ];
        let settings = NSDictionary::<NSString, AnyObject>::from_slices(
            &keys.iter().map(|k| &**k).collect::<Vec<_>>(),
            &values.iter().map(|v| v.as_ref()).collect::<Vec<_>>(),
        );
        let audio = AVMediaTypeAudio.ok_or("no audio media type")?;
        let has_sound = autoreleasepool(|_| asset(path).and_then(|a| track(&a, audio)).is_some());
        if !has_sound {
            return Ok(false);
        }
        let (reader, output) = open(path, audio, &settings, from)?;
        loop {
            let samples = autoreleasepool(|_| -> Option<Vec<f32>> {
                let sample = output.copyNextSampleBuffer()?;
                let Some(block) = sample.data_buffer() else {
                    return Some(Vec::new());
                };
                let len = block.data_length();
                let mut samples = vec![0f32; len / 4];
                let status = block.copy_data_bytes(0, samples.len() * 4, NonNull::from(&mut samples[..]).cast());
                Some(if status == 0 { samples } else { Vec::new() })
            });
            let Some(samples) = samples else {
                break;
            };
            if !samples.is_empty() && !each(samples) {
                reader.cancelReading();
                break;
            }
        }
        Ok(true)
    }
}

fn asset(path: &Path) -> Option<Retained<AVURLAsset>> {
    let url = NSURL::from_file_path(path)?;
    Some(unsafe { AVURLAsset::URLAssetWithURL_options(&url, None) })
}

fn track(asset: &AVURLAsset, kind: &AVMediaType) -> Option<Retained<AVAssetTrack>> {
    #[allow(deprecated)]
    unsafe { asset.tracksWithMediaType(kind) }.firstObject()
}

/// A reader of `path`'s first track of `kind` from `from` seconds on,
/// already reading.
unsafe fn open(
    path: &Path,
    kind: &AVMediaType,
    settings: &NSDictionary<NSString, AnyObject>,
    from: f64,
) -> Result<(Retained<AVAssetReader>, Retained<AVAssetReaderTrackOutput>), String> {
    unsafe {
        let asset = asset(path).ok_or("the file name isn't valid")?;
        let track = track(&asset, kind).ok_or("the file has no such track")?;
        let reader = AVAssetReader::assetReaderWithAsset_error(&asset)
            .map_err(|e| format!("couldn't open the video: {}", e.localizedDescription()))?;
        if from > 0.0 {
            reader.setTimeRange(CMTimeRange {
                start: CMTime::with_seconds(from, 600),
                duration: kCMTimePositiveInfinity,
            });
        }
        let output = AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(
            &track,
            Some(settings),
        );
        // The frames are read and let go of right away.
        output.setAlwaysCopiesSampleData(false);
        if !reader.canAddOutput(&output) {
            return Err("the video can't be decoded".into());
        }
        reader.addOutput(&output);
        if !reader.startReading() {
            let why = reader
                .error()
                .map(|e| e.localizedDescription().to_string())
                .unwrap_or_else(|| "no details".into());
            return Err(format!("couldn't read the video: {why}"));
        }
        Ok((reader, output))
    }
}

/// Core Video's CFString keys as the NSStrings they're bridged to.
fn cf_key(key: &'static CFString) -> &'static NSString {
    // SAFETY: CFString and NSString are toll-free bridged.
    unsafe { &*(key as *const CFString).cast::<NSString>() }
}
