//! A display's picture as it changes, through DXGI desktop duplication, for
//! recording. xcap's recorder isn't used for this on Windows: it duplicates
//! every display while looking for its own, which fails for one this
//! process is already duplicating, so it can't record two displays at once
//! (and, as its recorders are never let go of, can't record a second
//! display at all after recording one that comes before it). This
//! duplicates only the display it's for, until dropped.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{E_FAIL, RECT};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_UNSUPPORTED, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
    IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
};
use windows::core::Interface;

use crate::capture::Rect;

/// How often to try getting a lost capture back.
const RETRY: Duration = Duration::from_millis(250);

/// How long a display can be gone (or too small for the region) before the
/// recording gives up on it, e.g. while its mode changes.
const GIVE_UP: Duration = Duration::from_secs(10);

/// Getting a capture back after Windows takes it away, as it does when the
/// desktop switches (a UAC prompt, Ctrl+Alt+Del, the lock screen), the
/// display mode changes, a game goes fullscreen or the graphics driver
/// restarts. Until it's back, the recording repeats the last picture.
#[derive(Debug, Default)]
pub struct Recovery {
    lost_at: Option<Instant>,
    retry_at: Option<Instant>,
}

impl Recovery {
    /// The capture stopped with `e`.
    pub fn lost(&mut self, e: &windows::core::Error) {
        eprintln!("the screen capture was lost ({}); restarting it", e.message().trim());
        let now = Instant::now();
        self.lost_at = Some(now);
        self.retry_at = Some(now);
    }

    /// Whether it's time for another try.
    pub fn due(&self) -> bool {
        self.retry_at.is_none_or(|at| Instant::now() >= at)
    }

    /// The capture's back.
    pub fn back(&mut self) {
        if let Some(at) = self.lost_at.take() {
            eprintln!("the screen capture is back after {:.1?}", at.elapsed());
        }
        self.retry_at = None;
    }

    /// A try failed with `e`: `Ok` to keep trying, or the error to stop
    /// with once the display has been gone too long.
    pub fn failed(&mut self, e: &windows::core::Error) -> Result<(), String> {
        let gone = e.code() == E_FAIL || e.code() == DXGI_ERROR_UNSUPPORTED;
        if gone && self.lost_at.is_some_and(|at| at.elapsed() > GIVE_UP) {
            return Err(e.message().trim().to_string());
        }
        self.retry_at = Some(Instant::now() + RETRY);
        Ok(())
    }
}

/// The error for a display that's gone or now too small for the region,
/// which [`Recovery`] gives up on after a while.
pub fn display_gone(why: &str) -> windows::core::Error {
    windows::core::Error::new(E_FAIL, why)
}

/// Captures part of a display as it changes.
pub struct Duplicator {
    /// The display's top-left corner on the desktop.
    origin: (i32, i32),
    region: Rect,
    /// `None` while Windows has taken it away.
    capture: Option<Capture>,
    recovery: Recovery,
    /// The latest picture: RGBA.
    latest: Vec<u8>,
}

/// A running duplication of the display.
struct Capture {
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    /// Region-sized, readable copy of the screen.
    staging: ID3D11Texture2D,
}

impl Duplicator {
    /// Captures `region` (relative to the display) of the display whose
    /// top-left corner is at `origin`. Waits for its current picture.
    pub fn new(origin: (i32, i32), region: Rect) -> Result<Self, String> {
        let fail = |e: windows::core::Error| format!("couldn't capture the display: {}", e.message());
        // SAFETY: Direct3D calls on objects created here; the mapped
        // texture is read within its row pitch and unmapped.
        let capture = unsafe { open(origin, region) }.map_err(fail)?;
        let mut dup = Self {
            origin,
            region,
            capture: Some(capture),
            recovery: Recovery::default(),
            latest: vec![0; region.w as usize * region.h as usize * 4],
        };
        // A new duplication sends the whole desktop straight away.
        let deadline = Instant::now() + Duration::from_secs(1);
        while !unsafe { dup.take(250) }.map_err(fail)? {
            if Instant::now() > deadline {
                return Err(fail(DXGI_ERROR_WAIT_TIMEOUT.into()));
            }
        }
        Ok(dup)
    }

    /// Takes in the screen if it changed; `true` if it did. When Windows
    /// takes the capture away, it's restarted, and the picture stays as it
    /// was until then.
    pub fn poll(&mut self) -> Result<bool, String> {
        if self.capture.is_some() {
            // SAFETY: as in `new`.
            match unsafe { self.take(0) } {
                Ok(changed) => return Ok(changed),
                Err(e) => {
                    self.capture = None;
                    self.recovery.lost(&e);
                }
            }
        }
        if !self.recovery.due() {
            return Ok(false);
        }
        // SAFETY: as in `new`.
        match unsafe { open(self.origin, self.region) } {
            Ok(capture) => {
                self.capture = Some(capture);
                self.recovery.back();
                Ok(false)
            }
            Err(e) => self
                .recovery
                .failed(&e)
                .map(|()| false)
                .map_err(|why| format!("the screen capture stopped: {why}")),
        }
    }

    /// The latest picture of the region: RGBA.
    pub fn picture(&self) -> &[u8] {
        &self.latest
    }

    unsafe fn take(&mut self, timeout_ms: u32) -> windows::core::Result<bool> {
        unsafe {
            let Capture {
                context,
                duplication,
                staging,
            } = self.capture.as_ref().expect("running");
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource: Option<IDXGIResource> = None;
            match duplication.AcquireNextFrame(timeout_ms, &mut info, &mut resource) {
                Ok(()) => {}
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(false),
                Err(e) => return Err(e),
            }
            // Only the pointer moved: the picture is unchanged.
            if info.LastPresentTime == 0 {
                let _ = duplication.ReleaseFrame();
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
                context.CopySubresourceRegion(staging, 0, 0, 0, 0, &texture, 0, Some(&area));
                windows::core::Result::Ok(())
            })();
            let _ = duplication.ReleaseFrame();
            copied?;
            let mut mapped = Default::default();
            context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let row = self.region.w as usize * 4;
            for (y, dst) in self.latest.chunks_exact_mut(row).enumerate() {
                let src = std::slice::from_raw_parts((mapped.pData as *const u8).add(y * mapped.RowPitch as usize), row);
                for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                    d.copy_from_slice(&[s[2], s[1], s[0], 255]);
                }
            }
            context.Unmap(staging, 0);
            Ok(true)
        }
    }
}

/// Starts duplicating the display whose top-left corner is at `origin`,
/// with a readable texture the size of `region`.
unsafe fn open(origin: (i32, i32), region: Rect) -> windows::core::Result<Capture> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut a = 0;
        while let Ok(adapter) = factory.EnumAdapters1(a) {
            a += 1;
            let mut o = 0;
            while let Ok(output) = adapter.EnumOutputs(o) {
                o += 1;
                let desc = output.GetDesc()?;
                let RECT {
                    left,
                    top,
                    right,
                    bottom,
                } = desc.DesktopCoordinates;
                if !desc.AttachedToDesktop.as_bool() || (left, top) != origin {
                    continue;
                }
                let fits = region.x as i64 + region.w as i64 <= (right - left) as i64
                    && region.y as i64 + region.h as i64 <= (bottom - top) as i64;
                if !fits {
                    return Err(display_gone("the display's resolution is too small for the area now"));
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
                return Ok(Capture {
                    context,
                    duplication,
                    staging: staging.expect("created"),
                });
            }
        }
        Err(display_gone("the display isn't there any more"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_keeps_trying_and_gives_up_only_on_a_lost_display() {
        let mut r = Recovery::default();
        assert!(r.due());
        r.lost(&windows::Win32::Graphics::Dxgi::DXGI_ERROR_ACCESS_LOST.into());
        assert!(r.due());
        // Not allowed back yet (e.g. the lock screen): try again shortly.
        assert!(r.failed(&windows::Win32::Foundation::E_ACCESSDENIED.into()).is_ok());
        assert!(!r.due());
        // Gone for a moment, as while the display's mode changes: keep trying.
        assert!(r.failed(&display_gone("gone")).is_ok());
        // Gone for too long: give up, but not for anything else.
        r.lost_at = Some(Instant::now() - GIVE_UP - Duration::from_secs(1));
        assert!(r.failed(&windows::Win32::Foundation::E_ACCESSDENIED.into()).is_ok());
        assert_eq!(r.failed(&display_gone("gone")), Err("gone".into()));
        r.back();
        assert!(r.due());
        assert!(r.lost_at.is_none());
    }

    /// Records the primary display's corner for a few seconds; lock the PC,
    /// press Ctrl+Alt+Del or change the resolution meanwhile to check it
    /// carries on (`SNAPR_TEST_SECONDS` sets how long): `cargo test
    /// survives_losing_the_capture -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn survives_losing_the_capture() {
        let region = Rect {
            x: 0,
            y: 0,
            w: 200,
            h: 200,
        };
        let mut dup = Duplicator::new((0, 0), region).unwrap();
        let seconds = std::env::var("SNAPR_TEST_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(15);
        let end = Instant::now() + Duration::from_secs(seconds);
        let mut frames = 0;
        // Taken away part way through, as Windows does.
        let mut dropped = false;
        while Instant::now() < end {
            if !dropped && frames > 0 {
                dropped = true;
                dup.capture = None;
                dup.recovery.lost(&windows::Win32::Graphics::Dxgi::DXGI_ERROR_ACCESS_LOST.into());
            }
            frames += dup.poll().unwrap() as u32;
            std::thread::sleep(Duration::from_millis(16));
        }
        println!("{frames} frames");
        assert!(dup.capture.is_some(), "the capture came back");
    }
}
