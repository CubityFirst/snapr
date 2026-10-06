//! Decoding videos for the Recent page's player and previews, with what the
//! OS provides: Media Foundation on Windows, AVFoundation on macOS. Linux
//! has no system decoder, so FFmpeg does it there.

#[cfg(target_os = "macos")]
mod avf;
// Compiled everywhere so its tests can run on any machine.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod ffmpeg;
#[cfg(windows)]
mod mf;

use std::path::Path;

use image::RgbaImage;

#[cfg(target_os = "macos")]
use avf as platform;
#[cfg(not(any(windows, target_os = "macos")))]
use ffmpeg as platform;
#[cfg(windows)]
use mf as platform;

/// Frame rates above this are shown at this rate.
pub const MAX_FPS: f64 = 60.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Info {
    /// Seconds, if known.
    pub duration: Option<f64>,
    pub fps: f64,
}

/// A decoded frame: when it's shown (seconds) and the picture.
pub struct Picture {
    pub time: f64,
    pub image: RgbaImage,
}

/// The video's length and frame rate; `None` without a video track. `ffmpeg`
/// is the program to use where the OS has no decoder (Linux).
pub fn probe(path: &Path, ffmpeg: &str) -> Option<Info> {
    platform::probe(path, &program(ffmpeg))
}

/// Decodes pictures from `from` seconds on, shrunk to fit `max`, handing
/// each to `each` until it returns `false` or the video ends.
pub fn pictures(
    path: &Path,
    ffmpeg: &str,
    info: Info,
    from: f64,
    max: (u32, u32),
    each: &mut dyn FnMut(Picture) -> bool,
) -> Result<(), String> {
    platform::pictures(path, &program(ffmpeg), info, from, max, each)
}

/// Decodes the sound from `from` seconds on as interleaved f32 samples at
/// `rate` and `channels`, handing each chunk to `each` until it returns
/// `false` or the sound ends. `Ok(false)` if the video has no sound.
pub fn sound(
    path: &Path,
    ffmpeg: &str,
    from: f64,
    rate: u32,
    channels: u16,
    each: &mut dyn FnMut(Vec<f32>) -> bool,
) -> Result<bool, String> {
    platform::sound(path, &program(ffmpeg), from, rate, channels, each)
}

/// The video's first picture, shrunk to fit `max`.
pub fn first_picture(path: &Path, ffmpeg: &str, max: (u32, u32)) -> Option<RgbaImage> {
    let info = probe(path, ffmpeg)?;
    let mut first = None;
    pictures(path, ffmpeg, info, 0.0, max, &mut |p| {
        first = Some(p.image);
        false
    })
    .ok()?;
    first
}

fn program(ffmpeg: &str) -> String {
    match ffmpeg.trim() {
        "" => "ffmpeg".into(),
        p => p.into(),
    }
}

/// The size that fits `max` with the same shape, never larger than `w` x `h`.
pub fn fit(w: u32, h: u32, max: (u32, u32)) -> (u32, u32) {
    let scale = (max.0 as f64 / w as f64).min(max.1 as f64 / h as f64).min(1.0);
    (
        ((w as f64 * scale).round() as u32).max(1),
        ((h as f64 * scale).round() as u32).max(1),
    )
}

/// Where a decoded frame's pixels are in its buffer: `w` x `h` 4-byte
/// pixels, the top row starting at byte `top`, the next `stride` bytes on
/// (negative for bottom-up frames).
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
#[derive(Debug, Clone, Copy)]
pub(crate) struct Layout {
    pub w: u32,
    pub h: u32,
    pub top: usize,
    pub stride: isize,
}

/// Builds an RGBA picture of at most `max` from a decoded frame, with its
/// channels in `order` (indices of red, green and blue). Alpha is made
/// opaque. Shrinking samples each output pixel's area as 2x2 points, which
/// is quick and smooth enough for a preview.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub(crate) fn picture_from(
    data: &[u8],
    layout: Layout,
    order: [usize; 3],
    max: (u32, u32),
) -> Option<RgbaImage> {
    let Layout { w, h, top, stride } = layout;
    let (ow, oh) = fit(w, h, max);
    let row_len = w as usize * 4;
    let row = |y: u32| -> Option<&[u8]> {
        let start = usize::try_from(top as isize + y as isize * stride).ok()?;
        data.get(start..start + row_len)
    };
    let mut out = RgbaImage::new(ow, oh);
    if (ow, oh) == (w, h) {
        for (y, dst) in out.chunks_exact_mut(ow as usize * 4).enumerate() {
            let src = row(y as u32)?;
            for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                d.copy_from_slice(&[s[order[0]], s[order[1]], s[order[2]], 255]);
            }
        }
        return Some(out);
    }
    // Two sample points per axis, at a quarter and three quarters across
    // each output pixel.
    let points = |n: u32, size: u32| -> Vec<[u32; 2]> {
        (0..n)
            .map(|i| {
                let at = |f: f64| (((i as f64 + f) * size as f64 / n as f64) as u32).min(size - 1);
                [at(0.25), at(0.75)]
            })
            .collect()
    };
    let (xs, ys) = (points(ow, w), points(oh, h));
    for (y, dst) in out.chunks_exact_mut(ow as usize * 4).enumerate() {
        let (r0, r1) = (row(ys[y][0])?, row(ys[y][1])?);
        for (x, d) in dst.chunks_exact_mut(4).enumerate() {
            let [x0, x1] = xs[x].map(|v| v as usize * 4);
            for (c, &i) in order.iter().enumerate() {
                let sum = r0[x0 + i] as u32 + r0[x1 + i] as u32 + r1[x0 + i] as u32 + r1[x1 + i] as u32;
                d[c] = ((sum + 2) / 4) as u8;
            }
            d[3] = 255;
        }
    }
    Some(out)
}

/// How a decoder's YUV maps to RGB.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, Copy)]
pub(crate) struct Yuv {
    pub matrix: Matrix,
    /// 0-255 rather than the usual 16-235.
    pub full_range: bool,
    /// HDR10: PQ-coded BT.2020, shown mapped to SDR.
    pub pq: bool,
}

/// YUV coefficients.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Matrix {
    /// SD.
    Bt601,
    /// HD.
    Bt709,
    /// UHD and HDR.
    Bt2020,
}

/// Luminance HDR10 video shows SDR white at (ITU-R BT.2408).
const HDR_REFERENCE_WHITE_NITS: f32 = 203.0;

/// BT.2020 to BT.709 primaries, for linear light.
const TO_BT709: [[f32; 3]; 3] = [
    [1.660_491, -0.587_641, -0.072_850],
    [-0.124_550, 1.132_900, -0.008_349],
    [-0.018_151, -0.100_579, 1.118_730],
];

/// SMPTE ST 2084 (PQ) code (0..1) to nits.
fn pq_to_nits(e: f32) -> f32 {
    const M1: f32 = 0.159_301_76;
    const M2: f32 = 78.843_75;
    const C1: f32 = 0.835_937_5;
    const C2: f32 = 18.851_563;
    const C3: f32 = 18.6875;
    let p = e.clamp(0.0, 1.0).powf(1.0 / M2);
    10_000.0 * ((p - C1).max(0.0) / (C2 - C3 * p)).powf(1.0 / M1)
}

/// Linear light (1.0 = SDR white) to an sRGB code.
fn srgb_code(linear: f32) -> u8 {
    let c = linear.clamp(0.0, 1.0);
    let v = if c <= 0.003_130_8 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    (v * 255.0).round() as u8
}

/// PQ BT.2020 R'G'B' (0..1) to SDR sRGB: reference white becomes white,
/// brighter colours are scaled down by their brightest channel, keeping
/// their hue.
fn pq_to_srgb(rgb: [f32; 3]) -> [u8; 3] {
    let lin = rgb.map(|c| pq_to_nits(c) / HDR_REFERENCE_WHITE_NITS);
    let m = &TO_BT709;
    let rgb: [f32; 3] = std::array::from_fn(|r| (m[r][0] * lin[0] + m[r][1] * lin[1] + m[r][2] * lin[2]).max(0.0));
    let peak = rgb[0].max(rgb[1]).max(rgb[2]);
    let scale = if peak > 1.0 { 1.0 / peak } else { 1.0 };
    rgb.map(|c| srgb_code(c * scale))
}

/// Builds an RGBA picture of at most `max` from an NV12 frame: the Y plane
/// as `layout` (one byte per pixel; `stride` positive), then interleaved
/// U/V at half resolution starting at byte `uv_top`, with the same stride.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn picture_from_nv12(
    data: &[u8],
    layout: Layout,
    uv_top: usize,
    yuv: Yuv,
    max: (u32, u32),
) -> Option<RgbaImage> {
    let Layout { w, h, top, stride } = layout;
    let stride = usize::try_from(stride).ok()?;
    let (ow, oh) = fit(w, h, max);
    let (kr, kb) = match yuv.matrix {
        Matrix::Bt601 => (0.299, 0.114),
        Matrix::Bt709 => (0.2126, 0.0722),
        Matrix::Bt2020 => (0.2627, 0.0593),
    };
    let kg = 1.0 - kr - kb;
    let (y_scale, y_offset, c_scale) = if yuv.full_range {
        (1.0, 0.0, 1.0)
    } else {
        (255.0 / 219.0, 16.0, 255.0 / 224.0)
    };
    let luma = |x: u32, y: u32| -> Option<f32> { data.get(top + y as usize * stride + x as usize).map(|&v| v as f32) };
    let chroma = |x: u32, y: u32| -> Option<(f32, f32)> {
        let at = uv_top + (y / 2) as usize * stride + (x / 2 * 2) as usize;
        Some((*data.get(at)? as f32 - 128.0, *data.get(at + 1)? as f32 - 128.0))
    };
    // Two luma samples per axis, at a quarter and three quarters across
    // each output pixel (one sample each when not shrinking); chroma from
    // the middle.
    let points = |i: u32, n: u32, size: u32| -> [u32; 3] {
        let at = |f: f64| (((i as f64 + f) * size as f64 / n as f64) as u32).min(size - 1);
        if n == size { [i, i, i] } else { [at(0.25), at(0.75), at(0.5)] }
    };
    let mut out = RgbaImage::new(ow, oh);
    for oy in 0..oh {
        let [y0, y1, yc] = points(oy, oh, h);
        for ox in 0..ow {
            let [x0, x1, xc] = points(ox, ow, w);
            let l = (luma(x0, y0)? + luma(x1, y0)? + luma(x0, y1)? + luma(x1, y1)?) / 4.0;
            let (u, v) = chroma(xc, yc)?;
            let l = (l - y_offset) * y_scale;
            let (u, v) = (u * c_scale, v * c_scale);
            let r = l + 2.0 * (1.0 - kr) * v;
            let b = l + 2.0 * (1.0 - kb) * u;
            let g = l - (2.0 * kb * (1.0 - kb) * u + 2.0 * kr * (1.0 - kr) * v) / kg;
            let [r, g, b] = if yuv.pq {
                pq_to_srgb([r, g, b].map(|c| c / 255.0))
            } else {
                [r, g, b].map(|c| c.round().clamp(0.0, 255.0) as u8)
            };
            out.put_pixel(ox, oy, image::Rgba([r, g, b, 255]));
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_hdr10_to_sdr() {
        // Reference white (203 nits, PQ ~0.58) is white; black is black.
        assert_eq!(pq_to_srgb([0.5806; 3]), [255, 255, 255]);
        assert_eq!(pq_to_srgb([0.0; 3]), [0, 0, 0]);
        // 1000-nit white is still just white; a dim grey stays grey.
        assert_eq!(pq_to_srgb([0.7518; 3]), [255, 255, 255]);
        let grey = pq_to_srgb([0.4; 3]);
        assert!(grey[0] == grey[1] && grey[1] == grey[2] && (60..200).contains(&grey[0]), "{grey:?}");
        assert!((pq_to_nits(0.5081) - 100.0).abs() < 1.0);
    }

    #[test]
    fn converts_nv12() {
        let yuv = Yuv {
            matrix: Matrix::Bt709,
            full_range: false,
            pq: false,
        };
        // 2x2 of BT.709 limited-range red, then 2x2 white.
        for ([y, u, v], rgb) in [([63, 102, 240], [255, 0, 0]), ([235, 128, 128], [255, 255, 255])] {
            let data = [y, y, y, y, u, v];
            let layout = Layout {
                w: 2,
                h: 2,
                top: 0,
                stride: 2,
            };
            let img = picture_from_nv12(&data, layout, 4, yuv, (10, 10)).unwrap();
            for px in img.pixels() {
                for c in 0..3 {
                    assert!((px[c] as i32 - rgb[c] as i32).abs() <= 2, "{:?} vs {rgb:?}", px.0);
                }
            }
        }
    }

    /// Encodes clips with snapr's encoder and decodes them again (top half
    /// red, bottom half blue, one second with a tone):
    /// `cargo test decodes_own_recording -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn decodes_own_recording() {
        use crate::encode::{self, AudioFormat, VideoFormat};
        for (w, h) in [(320u32, 200u32), (1920, 1080)] {
            let path = std::env::temp_dir().join(format!("snapr-decode-{w}x{h}.mp4"));
            let video = VideoFormat {
                width: w,
                height: h,
                fps: 30,
            };
            let audio = AudioFormat {
                sample_rate: 48_000,
                channels: 2,
            };
            let mut enc = encode::open(&path, video, Some(audio), &Default::default()).unwrap();
            let frame: Vec<u8> = (0..w * h)
                .flat_map(|i| if i / w < h / 2 { [220, 30, 30, 255] } else { [30, 30, 220, 255] })
                .collect();
            for n in 0..30 {
                enc.video(&frame).unwrap();
                let tone: Vec<f32> = (0..1600)
                    .flat_map(|i| {
                        let s = ((n * 1600 + i) as f32 / 48_000.0 * 440.0 * std::f32::consts::TAU).sin() * 0.2;
                        [s, s]
                    })
                    .collect();
                enc.audio(&tone).unwrap();
            }
            enc.finish().unwrap();

            let info = probe(&path, "").expect("probe");
            println!("{w}x{h}: {info:?}");
            assert!((info.duration.unwrap() - 1.0).abs() < 0.1);
            assert!((info.fps - 30.0).abs() < 0.01);

            let mut times = Vec::new();
            let mut first = None;
            pictures(&path, "", info, 0.0, (1600, 1000), &mut |p| {
                times.push(p.time);
                first.get_or_insert(p.image);
                true
            })
            .unwrap();
            let first = first.unwrap();
            println!("  {} pictures, {:?}, first at {:.3}", times.len(), first.dimensions(), times[0]);
            assert_eq!(times.len(), 30);
            assert_eq!(first.dimensions(), fit(w, h, (1600, 1000)));
            let (fw, fh) = first.dimensions();
            let top = first.get_pixel(fw / 2, fh / 8).0;
            let bottom = first.get_pixel(fw / 2, fh * 7 / 8).0;
            println!("  top {top:?}, bottom {bottom:?}");
            assert!(top[0] > 180 && top[2] < 80, "top isn't red: {top:?}");
            assert!(bottom[2] > 180 && bottom[0] < 80, "bottom isn't blue: {bottom:?}");

            let mut seeked = None;
            pictures(&path, "", info, 0.5, (1600, 1000), &mut |p| {
                seeked = Some(p.time);
                false
            })
            .unwrap();
            println!("  seek to 0.5 starts at {seeked:?}");
            assert!((seeked.unwrap() - 0.5).abs() < 0.04);

            for (rate, channels, from) in [(48_000, 2, 0.0), (44_100, 1, 0.0), (48_000, 2, 0.5)] {
                let mut samples = 0;
                let had = sound(&path, "", from, rate, channels, &mut |s| {
                    samples += s.len();
                    true
                })
                .unwrap();
                let secs = samples as f64 / channels as f64 / rate as f64;
                println!("  sound at {rate} Hz x{channels} from {from}: {secs:.3} s");
                assert!(had);
                assert!((secs - (1.0 - from)).abs() < 0.06, "{secs}");
            }
        }
    }

    #[test]
    fn fits_inside_without_growing() {
        assert_eq!(fit(3840, 2160, (1600, 1000)), (1600, 900));
        assert_eq!(fit(1000, 2000, (1600, 1000)), (500, 1000));
        assert_eq!(fit(320, 200, (1600, 1000)), (320, 200));
    }

    #[test]
    fn converts_bgr_bottom_up() {
        // 2x2 BGRX, bottom row first: blue, green / red, white on top.
        let data = [
            255, 0, 0, 0, 0, 255, 0, 0, // bottom: blue, green
            0, 0, 255, 0, 255, 255, 255, 0, // top: red, white
        ];
        let layout = Layout {
            w: 2,
            h: 2,
            top: 8,
            stride: -8,
        };
        let img = picture_from(&data, layout, [2, 1, 0], (10, 10)).unwrap();
        assert_eq!(img.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(img.get_pixel(1, 0).0, [255, 255, 255, 255]);
        assert_eq!(img.get_pixel(0, 1).0, [0, 0, 255, 255]);
        assert_eq!(img.get_pixel(1, 1).0, [0, 255, 0, 255]);
    }

    #[test]
    fn shrinks_by_averaging() {
        // 4x2 RGBA: left half black, right half white; to 2x1.
        let px = |v: u8| [v, v, v, 255];
        let row = [px(0), px(0), px(255), px(255)].concat();
        let data = [row.clone(), row].concat();
        let layout = Layout {
            w: 4,
            h: 2,
            top: 0,
            stride: 16,
        };
        let img = picture_from(&data, layout, [0, 1, 2], (2, 1)).unwrap();
        assert_eq!(img.dimensions(), (2, 1));
        assert_eq!(img.get_pixel(0, 0).0, [0, 0, 0, 255]);
        assert_eq!(img.get_pixel(1, 0).0, [255, 255, 255, 255]);
    }
}
