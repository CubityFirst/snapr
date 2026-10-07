//! A display's picture as it changes, through DXGI desktop duplication, for
//! recording. xcap's recorder isn't used for this on Windows: it duplicates
//! every display while looking for its own, which fails for one this
//! process is already duplicating, so it can't record two displays at once
//! (and, as its recorders are never let go of, can't record a second
//! display at all after recording one that comes before it). This
//! duplicates only the display it's for, until dropped.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIFactory1,
    IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
};
use windows::core::Interface;

use crate::capture::Rect;

/// Captures part of a display as it changes.
pub struct Duplicator {
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    /// Region-sized, readable copy of the screen.
    staging: ID3D11Texture2D,
    region: Rect,
    /// The latest picture: RGBA.
    latest: Vec<u8>,
}

impl Duplicator {
    /// Captures `region` (relative to the display) of the display whose
    /// top-left corner is at `origin`. Waits for its current picture.
    pub fn new(origin: (i32, i32), region: Rect) -> Result<Self, String> {
        // SAFETY: Direct3D calls on objects created here; the mapped
        // texture is read within its row pitch and unmapped.
        unsafe { Self::open(origin, region) }
            .map_err(|e| format!("couldn't capture the display: {}", e.message()))
    }

    unsafe fn open(origin: (i32, i32), region: Rect) -> windows::core::Result<Self> {
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
            let mut a = 0;
            while let Ok(adapter) = factory.EnumAdapters1(a) {
                a += 1;
                let mut o = 0;
                while let Ok(output) = adapter.EnumOutputs(o) {
                    o += 1;
                    let desc = output.GetDesc()?;
                    let RECT { left, top, .. } = desc.DesktopCoordinates;
                    if !desc.AttachedToDesktop.as_bool() || (left, top) != origin {
                        continue;
                    }
                    let (device, context) = crate::hdr::device(&adapter)?;
                    let duplication = output.cast::<IDXGIOutput1>()?.DuplicateOutput(&device)?;
                    // Duplicated desktops are always BGRA.
                    let desc = D3D11_TEXTURE2D_DESC {
                        Width: region.w,
                        Height: region.h,
                        MipLevels: 1,
                        ArraySize: 1,
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
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
                    };
                    // A new duplication sends the whole desktop straight away.
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
                    while !dup.take(250)? {
                        if std::time::Instant::now() > deadline {
                            return Err(DXGI_ERROR_WAIT_TIMEOUT.into());
                        }
                    }
                    return Ok(dup);
                }
            }
            Err(windows::core::Error::new(
                windows::Win32::Foundation::E_FAIL,
                "the display isn't there any more",
            ))
        }
    }

    /// Takes in the screen if it changed; `true` if it did.
    pub fn poll(&mut self) -> Result<bool, String> {
        // SAFETY: as in `new`.
        unsafe { self.take(0) }.map_err(|e| format!("the screen capture stopped: {}", e.message()))
    }

    /// The latest picture of the region: RGBA.
    pub fn picture(&self) -> &[u8] {
        &self.latest
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
                let src = std::slice::from_raw_parts((mapped.pData as *const u8).add(y * mapped.RowPitch as usize), row);
                for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                    d.copy_from_slice(&[s[2], s[1], s[0], 255]);
                }
            }
            self.context.Unmap(&self.staging, 0);
            Ok(true)
        }
    }
}
