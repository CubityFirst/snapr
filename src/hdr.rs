//! Windows displays in HDR mode. Captured the usual way they can come out
//! washed out, so they're captured as 16-bit float (scRGB: linear, BT.709
//! primaries, 1.0 = 80 nits) and mapped to SDR here: the user's SDR
//! brightness becomes white, so ordinary windows look exactly as they do on
//! screen. Brighter HDR highlights can't be brighter than white in an SDR
//! picture; they're scaled to fit by their brightest channel, so they keep
//! their colour rather than turning white or shifting hue.

use image::RgbaImage;
use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SDR_WHITE_LEVEL, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DisplayConfigGetDeviceInfo,
    GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020, DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIAdapter1,
    IDXGIFactory1, IDXGIOutput5, IDXGIOutput6, IDXGIOutputDuplication, IDXGIResource,
};
use windows::core::Interface;

use crate::capture::Rect;
use crate::cursor;

/// A display in HDR mode.
pub struct HdrDisplay {
    /// Where it is on the desktop, in physical pixels.
    pub rect: Rect,
    /// The brightness of SDR white (the "SDR content brightness" setting).
    pub sdr_white_nits: f32,
    /// The display's peak brightness.
    pub peak_nits: f32,
    adapter: IDXGIAdapter1,
    output: IDXGIOutput5,
}

/// The displays currently in HDR mode.
pub fn displays() -> Vec<HdrDisplay> {
    // SAFETY: DXGI enumeration; everything is reference counted.
    unsafe { find() }.unwrap_or_default()
}

unsafe fn find() -> windows::core::Result<Vec<HdrDisplay>> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut found = Vec::new();
        let mut a = 0;
        while let Ok(adapter) = factory.EnumAdapters1(a) {
            a += 1;
            let mut o = 0;
            while let Ok(output) = adapter.EnumOutputs(o) {
                o += 1;
                let (Ok(output6), Ok(output5)) = (output.cast::<IDXGIOutput6>(), output.cast::<IDXGIOutput5>())
                else {
                    continue;
                };
                let Ok(desc) = output6.GetDesc1() else {
                    continue;
                };
                if !desc.AttachedToDesktop.as_bool() || desc.ColorSpace != DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020 {
                    continue;
                }
                let RECT { left, top, right, bottom } = desc.DesktopCoordinates;
                let name = String::from_utf16_lossy(&desc.DeviceName);
                found.push(HdrDisplay {
                    rect: Rect {
                        x: left,
                        y: top,
                        w: (right - left) as u32,
                        h: (bottom - top) as u32,
                    },
                    sdr_white_nits: sdr_white(name.trim_end_matches('\0')).unwrap_or(80.0),
                    peak_nits: desc.MaxLuminance.max(100.0),
                    adapter: adapter.clone(),
                    output: output5,
                });
            }
        }
        Ok(found)
    }
}

/// The SDR white level of the display with GDI name `device`
/// (`\\.\DISPLAY1`), in nits.
unsafe fn sdr_white(device: &str) -> Option<f32> {
    unsafe {
        let (mut paths, mut modes) = (0, 0);
        GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut paths, &mut modes).ok().ok()?;
        let mut path_info = vec![DISPLAYCONFIG_PATH_INFO::default(); paths as usize];
        let mut mode_info = vec![DISPLAYCONFIG_MODE_INFO::default(); modes as usize];
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut paths,
            path_info.as_mut_ptr(),
            &mut modes,
            mode_info.as_mut_ptr(),
            None,
        )
        .ok()
        .ok()?;
        for path in &path_info[..paths as usize] {
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                    adapterId: path.sourceInfo.adapterId,
                    id: path.sourceInfo.id,
                },
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut source.header) != 0 {
                continue;
            }
            let name = String::from_utf16_lossy(&source.viewGdiDeviceName);
            if name.trim_end_matches('\0') != device {
                continue;
            }
            let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
                    size: size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>() as u32,
                    adapterId: path.targetInfo.adapterId,
                    id: path.targetInfo.id,
                },
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut white.header) != 0 {
                return None;
            }
            // In thousandths of 80 nits.
            return Some(white.SDRWhiteLevel as f32 * 80.0 / 1000.0);
        }
        None
    }
}

impl HdrDisplay {
    /// The display's picture, tone-mapped to SDR.
    pub fn capture(&self) -> Result<RgbaImage, String> {
        let (w, h, pixels) = self.capture_scrgb()?;
        let mapping = ToneMap::new(self.sdr_white_nits);
        let mut out = RgbaImage::new(w, h);
        for (dst, src) in out.chunks_exact_mut(4).zip(pixels.chunks_exact(4)) {
            let rgb = mapping.apply([src[0], src[1], src[2]]);
            dst.copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
        Ok(out)
    }

    /// The display's picture as linear scRGB (RGBA floats).
    pub fn capture_scrgb(&self) -> Result<(u32, u32, Vec<f32>), String> {
        // SAFETY: Direct3D calls on objects created here; the mapped
        // texture is read within its row pitch and unmapped.
        unsafe { self.duplicate() }.map_err(|e| format!("couldn't capture the HDR display: {}", e.message()))
    }

    unsafe fn duplicate(&self) -> windows::core::Result<(u32, u32, Vec<f32>)> {
        unsafe {
            let (device, context) = device(&self.adapter)?;
            let duplication = self.output.DuplicateOutput1(&device, 0, &[DXGI_FORMAT_R16G16B16A16_FLOAT])?;
            // Frames that only move the pointer come with an empty picture;
            // wait for one with the desktop (a new duplication gets one
            // straight away).
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            loop {
                let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
                let mut resource: Option<IDXGIResource> = None;
                duplication.AcquireNextFrame(250, &mut info, &mut resource)?;
                if info.LastPresentTime == 0 {
                    let _ = duplication.ReleaseFrame();
                    if std::time::Instant::now() > deadline {
                        return Err(windows::Win32::Graphics::Dxgi::DXGI_ERROR_WAIT_TIMEOUT.into());
                    }
                    continue;
                }
                let result = (|| {
                    let texture: ID3D11Texture2D = resource.as_ref().expect("acquired").cast()?;
                    read_texture(&device, &context, &texture)
                })();
                let _ = duplication.ReleaseFrame();
                return result;
            }
        }
    }
}

impl HdrDisplay {
    /// A continuous capture of `region` (relative to the display), for
    /// recording. Waits for the display's current picture.
    pub fn duplicate_region(&self, region: Rect) -> Result<Duplicator, String> {
        // SAFETY: as in `duplicate`.
        unsafe { Duplicator::new(self, region) }
            .map_err(|e| format!("couldn't capture the HDR display: {}", e.message()))
    }
}

/// Captures part of an HDR display as it changes, as half floats (scRGB).
pub struct Duplicator {
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    /// Region-sized, readable copy of the screen.
    staging: ID3D11Texture2D,
    region: Rect,
    /// The latest picture: RGBA half floats.
    latest: Vec<u16>,
    pub sdr_white_nits: f32,
    pub peak_nits: f32,
}

impl Duplicator {
    unsafe fn new(display: &HdrDisplay, region: Rect) -> windows::core::Result<Self> {
        unsafe {
            let (device, context) = device(&display.adapter)?;
            let duplication = display.output.DuplicateOutput1(&device, 0, &[DXGI_FORMAT_R16G16B16A16_FLOAT])?;
            let desc = D3D11_TEXTURE2D_DESC {
                Width: region.w,
                Height: region.h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_R16G16B16A16_FLOAT,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut staging = None;
            device.CreateTexture2D(&desc, None, Some(&mut staging))?;
            let mut dup = Self {
                context,
                duplication,
                staging: staging.expect("created"),
                region,
                latest: vec![0; region.w as usize * region.h as usize * 4],
                sdr_white_nits: display.sdr_white_nits,
                peak_nits: display.peak_nits,
            };
            // A new duplication sends the whole desktop straight away.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            while !dup.take(250)? {
                if std::time::Instant::now() > deadline {
                    return Err(DXGI_ERROR_WAIT_TIMEOUT.into());
                }
            }
            Ok(dup)
        }
    }

    /// Takes in the screen if it changed within `timeout_ms`; `true` if it
    /// did.
    pub fn poll(&mut self, timeout_ms: u32) -> Result<bool, String> {
        // SAFETY: as in `duplicate`.
        unsafe { self.take(timeout_ms) }.map_err(|e| format!("the HDR capture stopped: {}", e.message()))
    }

    unsafe fn take(&mut self, timeout_ms: u32) -> windows::core::Result<bool> {
        unsafe {
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource: Option<IDXGIResource> = None;
            match self.duplication.AcquireNextFrame(timeout_ms, &mut info, &mut resource) {
                Ok(()) => {}
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(false),
                Err(e) => return Err(e),
            }
            // Only the pointer moved: the picture is unchanged.
            if info.LastPresentTime == 0 {
                let _ = self.duplication.ReleaseFrame();
                return Ok(false);
            }
            let copied = (|| {
                let texture: ID3D11Texture2D = resource.as_ref().expect("acquired").cast()?;
                let r = self.region;
                let area = D3D11_BOX {
                    left: r.x as u32,
                    top: r.y as u32,
                    front: 0,
                    right: r.x as u32 + r.w,
                    bottom: r.y as u32 + r.h,
                    back: 1,
                };
                self.context.CopySubresourceRegion(&self.staging, 0, 0, 0, 0, &texture, 0, Some(&area));
                windows::core::Result::Ok(())
            })();
            let _ = self.duplication.ReleaseFrame();
            copied?;
            let mut mapped = Default::default();
            self.context.Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let row = self.region.w as usize * 4;
            for (y, dst) in self.latest.chunks_exact_mut(row).enumerate() {
                let src = (mapped.pData as *const u8).add(y * mapped.RowPitch as usize) as *const u16;
                dst.copy_from_slice(std::slice::from_raw_parts(src, row));
            }
            self.context.Unmap(&self.staging, 0);
            Ok(true)
        }
    }

    /// The latest picture, tone-mapped to SDR (for a preview).
    pub fn sdr_picture(&self) -> RgbaImage {
        let mapping = ToneMap::new(self.sdr_white_nits);
        let mut out = RgbaImage::new(self.region.w, self.region.h);
        for (dst, src) in out.chunks_exact_mut(4).zip(self.latest.chunks_exact(4)) {
            let rgb = mapping.apply([f16_to_f32(src[0]), f16_to_f32(src[1]), f16_to_f32(src[2])]);
            dst.copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
        out
    }

    /// The latest picture as HDR10 P010 (see `to_p010`), with the pointer
    /// drawn in at `cursor` (its top-left, relative to the region).
    pub fn p010(&self, cursor: Option<((i32, i32), &cursor::Image)>, out: &mut Vec<u8>) {
        to_p010(&self.latest, self.region.w as usize, self.region.h as usize, self.sdr_white_nits, cursor, out);
    }
}

/// BT.709 to BT.2020 primaries, for linear light.
const TO_BT2020: [[f32; 3]; 3] = [
    [0.627_404, 0.329_283, 0.043_313],
    [0.069_097, 0.919_540, 0.011_362],
    [0.016_391, 0.088_013, 0.895_595],
];

/// Lookup tables for `to_p010`.
struct Tables {
    /// Every half float as f32.
    half: Vec<f32>,
    /// PQ code (0..1) of nits/10000, sampled evenly in its fourth root.
    pq: Vec<f32>,
    /// sRGB code (0..255) to linear.
    srgb: [f32; 256],
}

const PQ_STEPS: usize = 4096;

fn tables() -> &'static Tables {
    static TABLES: std::sync::OnceLock<Tables> = std::sync::OnceLock::new();
    TABLES.get_or_init(|| Tables {
        half: (0..=u16::MAX).map(f16_to_f32).collect(),
        pq: (0..=PQ_STEPS)
            .map(|i| pq_encode((i as f32 / PQ_STEPS as f32).powi(4)))
            .collect(),
        srgb: std::array::from_fn(|i| {
            let c = i as f32 / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        }),
    })
}

/// SMPTE ST 2084 (PQ) of a luminance as a fraction of 10000 nits.
fn pq_encode(y: f32) -> f32 {
    const M1: f32 = 0.159_301_76;
    const M2: f32 = 78.843_75;
    const C1: f32 = 0.835_937_5;
    const C2: f32 = 18.851_563;
    const C3: f32 = 18.6875;
    let p = y.clamp(0.0, 1.0).powf(M1);
    ((C1 + C2 * p) / (1.0 + C3 * p)).powf(M2)
}

fn pq_lookup(t: &Tables, y: f32) -> f32 {
    let x = y.clamp(0.0, 1.0).sqrt().sqrt() * PQ_STEPS as f32;
    let i = (x as usize).min(PQ_STEPS - 1);
    let f = x - i as f32;
    t.pq[i] + (t.pq[i + 1] - t.pq[i]) * f
}

/// RGBA half floats (scRGB) to HDR10 P010: 10-bit BT.2020 PQ, limited
/// range; a Y plane, then interleaved U/V at half resolution, each value in
/// the top 10 bits of a little-endian u16. `w` and `h` are even. The
/// pointer, if given, is drawn in at SDR white.
pub fn to_p010(
    src: &[u16],
    w: usize,
    h: usize,
    sdr_white_nits: f32,
    cursor: Option<((i32, i32), &cursor::Image)>,
    out: &mut Vec<u8>,
) {
    let t = tables();
    out.resize(w * h * 3, 0); // 1.5 samples per pixel, 2 bytes each
    let (y_plane, uv_plane) = out.split_at_mut(w * h * 2);
    let white = sdr_white_nits / 80.0;
    // PQ-coded BT.2020 R'G'B' of one pixel.
    let pixel = |x: usize, y: usize| -> [f32; 3] {
        let i = (y * w + x) * 4;
        let mut rgb = [t.half[src[i] as usize], t.half[src[i + 1] as usize], t.half[src[i + 2] as usize]];
        if let Some(((cx, cy), img)) = cursor {
            let (px, py) = (x as i32 - cx, y as i32 - cy);
            if px >= 0 && py >= 0 && (px as u32) < img.width && (py as u32) < img.height {
                let s = (py as usize * img.width as usize + px as usize) * 4;
                let a = img.pixels[s + 3] as f32 / 255.0;
                if a > 0.0 {
                    for (c, v) in rgb.iter_mut().enumerate() {
                        // Premultiplied: unpremultiply to linearize.
                        let straight = (img.pixels[s + c] as f32 / a).round().min(255.0) as usize;
                        *v = t.srgb[straight] * white * a + *v * (1.0 - a);
                    }
                }
            }
        }
        let m = &TO_BT2020;
        std::array::from_fn(|r| {
            let lin = m[r][0] * rgb[0] + m[r][1] * rgb[1] + m[r][2] * rgb[2];
            // scRGB 1.0 is 80 nits.
            pq_lookup(t, lin * 80.0 / 10_000.0)
        })
    };
    let luma = |p: [f32; 3]| 0.2627 * p[0] + 0.6780 * p[1] + 0.0593 * p[2];
    let code = |v: f32, offset: f32, scale: f32| {
        (((offset + scale * v).round().clamp(0.0, 1023.0) as u16) << 6).to_le_bytes()
    };
    // Rows in pairs (one chroma row each), spread over the cores.
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8);
    let per = (h / 2).div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        for (n, (ys, uvs)) in y_plane
            .chunks_mut(per * 2 * w * 2)
            .zip(uv_plane.chunks_mut(per * w * 2))
            .enumerate()
        {
            scope.spawn(move || {
                let mut sum = vec![[0f32; 3]; w / 2];
                for (k, uv_row) in uvs.chunks_exact_mut(w * 2).enumerate() {
                    let y0 = (n * per + k) * 2;
                    sum.iter_mut().for_each(|s| *s = [0.0; 3]);
                    for dy in 0..2 {
                        let row = &mut ys[(k * 2 + dy) * w * 2..(k * 2 + dy + 1) * w * 2];
                        for x in 0..w {
                            let p = pixel(x, y0 + dy);
                            row[x * 2..x * 2 + 2].copy_from_slice(&code(luma(p), 64.0, 876.0));
                            let s = &mut sum[x / 2];
                            for c in 0..3 {
                                s[c] += p[c] / 4.0;
                            }
                        }
                    }
                    for (x, p) in sum.iter().enumerate() {
                        let yl = luma(*p);
                        let cb = (p[2] - yl) / 1.8814;
                        let cr = (p[0] - yl) / 1.4746;
                        uv_row[x * 4..x * 4 + 2].copy_from_slice(&code(cb, 512.0, 896.0));
                        uv_row[x * 4 + 2..x * 4 + 4].copy_from_slice(&code(cr, 512.0, 896.0));
                    }
                }
            });
        }
    });
}

/// A Direct3D device on `adapter`.
pub(crate) unsafe fn device(adapter: &IDXGIAdapter1) -> windows::core::Result<(ID3D11Device, ID3D11DeviceContext)> {
    unsafe {
        let (mut device, mut context) = (None, None);
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
        Ok((device.expect("created"), context.expect("created")))
    }
}

/// Copies an FP16 texture to memory as floats.
unsafe fn read_texture(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    texture: &ID3D11Texture2D,
) -> windows::core::Result<(u32, u32, Vec<f32>)> {
    unsafe {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        texture.GetDesc(&mut desc);
        let staging_desc = D3D11_TEXTURE2D_DESC {
            Width: desc.Width,
            Height: desc.Height,
            MipLevels: 1,
            ArraySize: 1,
            Format: desc.Format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging = None;
        device.CreateTexture2D(&staging_desc, None, Some(&mut staging))?;
        let staging = staging.expect("created");
        context.CopyResource(&staging, texture);
        let mut mapped = Default::default();
        context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        let (w, h) = (desc.Width as usize, desc.Height as usize);
        let mut pixels = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            let row = std::slice::from_raw_parts((mapped.pData as *const u8).add(y * mapped.RowPitch as usize), w * 8);
            pixels.extend(row.chunks_exact(2).map(|b| f16_to_f32(u16::from_le_bytes([b[0], b[1]]))));
        }
        context.Unmap(&staging, 0);
        Ok((w as u32, h as u32, pixels))
    }
}

/// Half-precision float to f32.
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = (h >> 10) & 0x1f;
    let frac = (h & 0x3ff) as f32;
    sign * match exp {
        0 => frac * 2f32.powi(-24),
        31 => {
            if frac == 0.0 {
                f32::INFINITY
            } else {
                f32::NAN
            }
        }
        e => (1.0 + frac / 1024.0) * 2f32.powi(e as i32 - 15),
    }
}

/// scRGB to 8-bit sRGB, with SDR white as full brightness.
#[derive(Debug, Clone, Copy)]
pub struct ToneMap {
    /// scRGB value of SDR white.
    white: f32,
}

impl ToneMap {
    pub fn new(sdr_white_nits: f32) -> Self {
        Self {
            white: sdr_white_nits.max(1.0) / 80.0,
        }
    }

    pub fn apply(&self, rgb: [f32; 3]) -> [u8; 3] {
        // Out-of-gamut (negative) parts are dropped.
        let rgb = rgb.map(|c| (c / self.white).max(0.0));
        // Brighter than white: scale down by the largest channel, keeping
        // the hue.
        let m = rgb[0].max(rgb[1]).max(rgb[2]);
        let scale = if m > 1.0 { 1.0 / m } else { 1.0 };
        rgb.map(|c| srgb_encode(c * scale))
    }
}

fn srgb_encode(linear: f32) -> u8 {
    let c = linear.clamp(0.0, 1.0);
    let v = if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (v * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_halves() {
        assert_eq!(f16_to_f32(0x3C00), 1.0);
        assert_eq!(f16_to_f32(0x0000), 0.0);
        assert_eq!(f16_to_f32(0xC000), -2.0);
        assert_eq!(f16_to_f32(0x3555), 0.333_251_95);
        assert_eq!(f16_to_f32(0x7BFF), 65504.0);
    }

    #[test]
    fn pq_matches_the_standard() {
        // ST 2084 reference points: 100 nits ~ 0.508, 1000 nits ~ 0.752.
        assert!((pq_encode(0.01) - 0.5081).abs() < 0.001);
        assert!((pq_encode(0.1) - 0.7518).abs() < 0.001);
        assert!((pq_encode(1.0) - 1.0).abs() < 1e-6);
        let t = tables();
        for y in [0.0001, 0.003, 0.02, 0.24, 0.9] {
            assert!((pq_lookup(t, y) - pq_encode(y)).abs() < 0.002, "{y}");
        }
    }

    #[test]
    fn p010_of_sdr_white_and_black() {
        // 2x2 of SDR white (240 nits: scRGB 3.0 = half 0x4200), then black.
        for (half, y_code) in [(0x4200u16, 64.0 + 876.0 * pq_encode(0.024)), (0, 64.0)] {
            let src: Vec<u16> = (0..4).flat_map(|_| [half, half, half, 0x3C00]).collect();
            let mut out = Vec::new();
            to_p010(&src, 2, 2, 240.0, None, &mut out);
            let word = |i: usize| u16::from_le_bytes([out[i * 2], out[i * 2 + 1]]) >> 6;
            assert!((word(0) as f32 - y_code).abs() <= 1.0, "{} vs {y_code}", word(0));
            // Grey: no colour.
            assert_eq!((word(4), word(5)), (512, 512));
        }
    }

    #[test]
    fn sdr_content_is_unchanged() {
        // SDR brightness at 240 nits: scRGB 3.0 is white.
        let t = ToneMap::new(240.0);
        assert_eq!(t.apply([0.0, 0.0, 0.0]), [0, 0, 0]);
        assert_eq!(t.apply([3.0; 3]), [255, 255, 255]);
        // Mid grey (sRGB 128 = linear 0.2158).
        assert_eq!(t.apply([0.2158 * 3.0; 3]), [128, 128, 128]);
    }

    #[test]
    fn highlights_keep_their_colour() {
        let t = ToneMap::new(200.0);
        let white = 200.0 / 80.0;
        assert_eq!(t.apply([white * 5.0; 3]), [255, 255, 255]);
        // A bright orange stays orange instead of turning yellow-white.
        let orange = t.apply([white * 3.0, white * 1.5, 0.0]);
        assert_eq!(orange[0], 255);
        assert!((180..195).contains(&orange[1]) && orange[2] == 0, "{orange:?}");
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// Captures each display in HDR mode to `%TEMP%/snapr-hdr-<n>.png`,
    /// next to the plain capture for comparison:
    /// `cargo test hdr_capture -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn hdr_capture() {
        // Like snapr itself (winit sets it); duplication needs it.
        unsafe {
            let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
                windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            );
        }
        let found = displays();
        println!("{} display(s) in HDR mode", found.len());
        for (n, d) in found.iter().enumerate() {
            println!("  {:?}: SDR white {} nits, peak {} nits", d.rect, d.sdr_white_nits, d.peak_nits);
            let mapped = d.capture().unwrap();
            let plain = xcap::Monitor::all()
                .unwrap()
                .into_iter()
                .find(|m| (m.x().unwrap(), m.y().unwrap()) == (d.rect.x, d.rect.y))
                .unwrap()
                .capture_image()
                .unwrap();
            let dir = std::env::temp_dir();
            mapped.save(dir.join(format!("snapr-hdr-{n}.png"))).unwrap();
            plain.save(dir.join(format!("snapr-hdr-{n}-plain.png"))).unwrap();
            let mean = |img: &RgbaImage| img.pixels().map(|p| p[0] as f64 + p[1] as f64 + p[2] as f64).sum::<f64>() / (img.len() as f64 * 0.75);
            println!("  mean level: mapped {:.1}, plain {:.1}", mean(&mapped), mean(&plain));
        }
    }
}
