//! macOS: AVFoundation's asset writer. VideoToolbox does the H.264 (on the
//! GPU) and Core Audio the AAC.

use std::path::Path;
use std::ptr::{self, NonNull};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2_av_foundation::{
    AVAssetWriter, AVAssetWriterInput, AVAssetWriterInputPixelBufferAdaptor, AVAssetWriterStatus,
    AVFileTypeMPEG4, AVMediaTypeAudio, AVMediaTypeVideo, AVVideoAverageBitRateKey,
    AVVideoCodecKey, AVVideoCodecTypeH264, AVVideoCompressionPropertiesKey, AVVideoHeightKey,
    AVVideoProfileLevelH264HighAutoLevel, AVVideoProfileLevelKey, AVVideoWidthKey,
};
use objc2_core_audio_types::{
    AudioStreamBasicDescription, kAudioFormatFlagIsFloat, kAudioFormatFlagIsPacked,
    kAudioFormatLinearPCM, kAudioFormatMPEG4AAC,
};
use objc2_core_foundation::{CFRetained, CFString};
use objc2_core_media::{
    CMAudioFormatDescription, CMAudioFormatDescriptionCreate,
    CMAudioSampleBufferCreateReadyWithPacketDescriptions, CMBlockBuffer, CMSampleBuffer, CMTime,
    kCMBlockBufferAssureMemoryNowFlag, kCMTimeZero,
};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferPool,
    CVPixelBufferUnlockBaseAddress, kCVPixelBufferHeightKey, kCVPixelBufferPixelFormatTypeKey,
    kCVPixelBufferWidthKey, kCVPixelFormatType_32BGRA,
};
use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};

use super::{AudioFormat, Encoder, VideoFormat, video_bitrate};

/// AAC bit rate.
const AAC_BITS_PER_SECOND: u32 = 160_000;

/// How long to wait for an input to take more data before giving up.
const READY_TIMEOUT: Duration = Duration::from_secs(2);

pub struct AssetWriter {
    writer: Retained<AVAssetWriter>,
    video_input: Retained<AVAssetWriterInput>,
    adaptor: Retained<AVAssetWriterInputPixelBufferAdaptor>,
    audio: Option<Audio>,
    video: VideoFormat,
    frames: i64,
    finished: bool,
}

struct Audio {
    input: Retained<AVAssetWriterInput>,
    description: CFRetained<CMAudioFormatDescription>,
    format: AudioFormat,
    samples: i64,
}

impl AssetWriter {
    pub fn open(out: &Path, video: VideoFormat, audio: Option<AudioFormat>) -> Result<Self, String> {
        autoreleasepool(|_| unsafe { Self::create(out, video, audio) })
    }

    unsafe fn create(out: &Path, video: VideoFormat, audio: Option<AudioFormat>) -> Result<Self, String> {
        unsafe {
            // The writer refuses to replace a file.
            let _ = std::fs::remove_file(out);
            let url = NSURL::from_file_path(out).ok_or("the file name isn't valid")?;
            let file_type = AVFileTypeMPEG4.ok_or("MP4 isn't supported")?;
            let writer = AVAssetWriter::assetWriterWithURL_fileType_error(&url, file_type)
                .map_err(|e| format!("couldn't start the video file: {}", e.localizedDescription()))?;
            writer.setShouldOptimizeForNetworkUse(true);

            let key = |k: Option<&'static NSString>| k.expect("AVFoundation key");
            let compression = NSDictionary::<NSString, AnyObject>::from_slices(
                &[key(AVVideoAverageBitRateKey), key(AVVideoProfileLevelKey)],
                &[
                    NSNumber::new_u32(video_bitrate(video)).as_ref(),
                    key(AVVideoProfileLevelH264HighAutoLevel).as_ref(),
                ],
            );
            let (width, height) = (NSNumber::new_u32(video.width), NSNumber::new_u32(video.height));
            let settings = NSDictionary::<NSString, AnyObject>::from_slices(
                &[
                    key(AVVideoCodecKey),
                    key(AVVideoWidthKey),
                    key(AVVideoHeightKey),
                    key(AVVideoCompressionPropertiesKey),
                ],
                &[
                    key(AVVideoCodecTypeH264).as_ref(),
                    width.as_ref(),
                    height.as_ref(),
                    compression.as_ref(),
                ],
            );
            let video_input = AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
                AVMediaTypeVideo.ok_or("no video media type")?,
                Some(&settings),
            );
            video_input.setExpectsMediaDataInRealTime(true);
            if !writer.canAddInput(&video_input) {
                return Err("the video encoder doesn't take these settings".into());
            }
            writer.addInput(&video_input);
            let source = NSDictionary::<NSString, AnyObject>::from_slices(
                &[
                    cf_key(kCVPixelBufferPixelFormatTypeKey),
                    cf_key(kCVPixelBufferWidthKey),
                    cf_key(kCVPixelBufferHeightKey),
                ],
                &[
                    NSNumber::new_u32(kCVPixelFormatType_32BGRA).as_ref(),
                    width.as_ref(),
                    height.as_ref(),
                ],
            );
            let adaptor = AVAssetWriterInputPixelBufferAdaptor::assetWriterInputPixelBufferAdaptorWithAssetWriterInput_sourcePixelBufferAttributes(
                &video_input,
                Some(&source),
            );

            let audio = match audio {
                Some(format) => Some(Audio::create(&writer, format)?),
                None => None,
            };
            if !writer.startWriting() {
                return Err(format!("couldn't start the video file: {}", error(&writer)));
            }
            writer.startSessionAtSourceTime(kCMTimeZero);
            Ok(Self {
                writer,
                video_input,
                adaptor,
                audio,
                video,
                frames: 0,
                finished: false,
            })
        }
    }

    fn pool(&self) -> Result<Retained<CVPixelBufferPool>, String> {
        unsafe { self.adaptor.pixelBufferPool() }
            .ok_or_else(|| format!("the video encoder stopped: {}", error(&self.writer)))
    }
}

impl Audio {
    unsafe fn create(writer: &AVAssetWriter, format: AudioFormat) -> Result<Self, String> {
        unsafe {
            let settings = NSDictionary::<NSString, AnyObject>::from_slices(
                // AVFAudio's key constants, by value.
                &[
                    &*NSString::from_str("AVFormatIDKey"),
                    &*NSString::from_str("AVSampleRateKey"),
                    &*NSString::from_str("AVNumberOfChannelsKey"),
                    &*NSString::from_str("AVEncoderBitRateKey"),
                ],
                &[
                    NSNumber::new_u32(kAudioFormatMPEG4AAC).as_ref(),
                    NSNumber::new_f64(format.sample_rate as f64).as_ref(),
                    NSNumber::new_u32(format.channels as u32).as_ref(),
                    NSNumber::new_u32(AAC_BITS_PER_SECOND).as_ref(),
                ],
            );
            let input = AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
                AVMediaTypeAudio.ok_or("no audio media type")?,
                Some(&settings),
            );
            input.setExpectsMediaDataInRealTime(true);
            if !writer.canAddInput(&input) {
                return Err("the audio encoder doesn't take these settings".into());
            }
            writer.addInput(&input);

            // What's appended: interleaved f32.
            let channels = format.channels as u32;
            let asbd = AudioStreamBasicDescription {
                mSampleRate: format.sample_rate as f64,
                mFormatID: kAudioFormatLinearPCM,
                mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
                mBytesPerPacket: 4 * channels,
                mFramesPerPacket: 1,
                mBytesPerFrame: 4 * channels,
                mChannelsPerFrame: channels,
                mBitsPerChannel: 32,
                mReserved: 0,
            };
            let mut description = ptr::null();
            let status = CMAudioFormatDescriptionCreate(
                None,
                NonNull::from(&asbd),
                0,
                ptr::null(),
                0,
                ptr::null(),
                None,
                NonNull::from(&mut description),
            );
            let description = NonNull::new(description.cast_mut())
                .filter(|_| status == 0)
                .ok_or_else(|| format!("couldn't describe the audio format ({status})"))?;
            Ok(Self {
                input,
                description: CFRetained::from_raw(description),
                format,
                samples: 0,
            })
        }
    }
}

impl Encoder for AssetWriter {
    fn video(&mut self, rgba: &[u8]) -> Result<(), String> {
        autoreleasepool(|_| unsafe {
            let pool = self.pool()?;
            let mut buffer = ptr::null_mut();
            let status = CVPixelBufferPool::create_pixel_buffer(None, &pool, NonNull::from(&mut buffer));
            let buffer: CFRetained<CVPixelBuffer> = NonNull::new(buffer)
                .filter(|_| status == 0)
                .map(|b| CFRetained::from_raw(b))
                .ok_or_else(|| format!("couldn't get a video buffer ({status})"))?;
            CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags(0));
            let base = CVPixelBufferGetBaseAddress(&buffer).cast::<u8>();
            let stride = CVPixelBufferGetBytesPerRow(&buffer);
            let row = self.video.width as usize * 4;
            for (y, src) in rgba.chunks_exact(row).take(self.video.height as usize).enumerate() {
                let dst = std::slice::from_raw_parts_mut(base.add(y * stride), row);
                for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                    d.copy_from_slice(&[s[2], s[1], s[0], 255]);
                }
            }
            CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags(0));
            wait_ready(&self.video_input)?;
            let time = CMTime::new(self.frames, self.video.fps.max(1) as i32);
            if !self.adaptor.appendPixelBuffer_withPresentationTime(&buffer, time) {
                return Err(format!("the video encoder failed: {}", error(&self.writer)));
            }
            self.frames += 1;
            Ok(())
        })
    }

    fn audio(&mut self, samples: &[f32]) -> Result<(), String> {
        let Some(audio) = &mut self.audio else {
            return Ok(());
        };
        let channels = audio.format.channels.max(1) as usize;
        let frames = samples.len() / channels;
        if frames == 0 {
            return Ok(());
        }
        let bytes = frames * channels * 4;
        autoreleasepool(|_| unsafe {
            let mut block = ptr::null_mut();
            let status = CMBlockBuffer::create_with_memory_block(
                None,
                ptr::null_mut(),
                bytes,
                None,
                ptr::null(),
                0,
                bytes,
                kCMBlockBufferAssureMemoryNowFlag,
                NonNull::from(&mut block),
            );
            let block: CFRetained<CMBlockBuffer> = NonNull::new(block)
                .filter(|_| status == 0)
                .map(|b| CFRetained::from_raw(b))
                .ok_or_else(|| format!("couldn't get an audio buffer ({status})"))?;
            let status = CMBlockBuffer::replace_data_bytes(
                NonNull::new_unchecked(samples.as_ptr().cast_mut().cast()),
                &block,
                0,
                bytes,
            );
            if status != 0 {
                return Err(format!("couldn't fill an audio buffer ({status})"));
            }
            let mut sample = ptr::null_mut();
            let status = CMAudioSampleBufferCreateReadyWithPacketDescriptions(
                None,
                &block,
                &audio.description,
                frames as isize,
                CMTime::new(audio.samples, audio.format.sample_rate as i32),
                ptr::null(),
                NonNull::from(&mut sample),
            );
            let sample: CFRetained<CMSampleBuffer> = NonNull::new(sample)
                .filter(|_| status == 0)
                .map(|s| CFRetained::from_raw(s))
                .ok_or_else(|| format!("couldn't make an audio sample ({status})"))?;
            wait_ready(&audio.input)?;
            if !audio.input.appendSampleBuffer(&sample) {
                return Err(format!("the audio encoder failed: {}", error(&self.writer)));
            }
            audio.samples += frames as i64;
            Ok(())
        })
    }

    fn finish(mut self: Box<Self>) -> Result<Option<String>, String> {
        self.finished = true;
        let (tx, rx) = mpsc::channel();
        unsafe {
            self.video_input.markAsFinished();
            if let Some(audio) = &self.audio {
                audio.input.markAsFinished();
            }
            let done = RcBlock::new(move || {
                let _ = tx.send(());
            });
            self.writer.finishWritingWithCompletionHandler(&done);
        }
        rx.recv().map_err(|_| "the video file wasn't finished".to_string())?;
        if unsafe { self.writer.status() } == AVAssetWriterStatus::Completed {
            Ok(None)
        } else {
            Err(format!("couldn't finish the video file: {}", error(&self.writer)))
        }
    }
}

impl Drop for AssetWriter {
    fn drop(&mut self) {
        if !self.finished {
            unsafe { self.writer.cancelWriting() };
        }
    }
}

/// Core Video's CFString keys as the NSStrings they're bridged to.
fn cf_key(key: &'static CFString) -> &'static NSString {
    // SAFETY: CFString and NSString are toll-free bridged.
    unsafe { &*(key as *const CFString).cast::<NSString>() }
}

/// Waits until `input` takes more data (in real-time mode it says no while
/// the encoder catches up).
fn wait_ready(input: &AVAssetWriterInput) -> Result<(), String> {
    let deadline = Instant::now() + READY_TIMEOUT;
    while !unsafe { input.isReadyForMoreMediaData() } {
        if Instant::now() > deadline {
            return Err("the encoder stopped taking data".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

fn error(writer: &AVAssetWriter) -> String {
    unsafe { writer.error() }
        .map(|e| e.localizedDescription().to_string())
        .unwrap_or_else(|| "no details".into())
}
