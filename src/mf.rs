//! Windows Media Foundation set-up shared by the encoder and the decoder.

use windows::Win32::Media::MediaFoundation::{IMFAttributes, MF_VERSION, MFCreateAttributes, MFSTARTUP_FULL, MFShutdown, MFStartup};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

/// COM and Media Foundation, started for the current thread until dropped.
/// Media Foundation objects must be released before it.
pub struct Session {
    com: bool,
}

impl Session {
    pub fn start() -> Result<Self, String> {
        // SAFETY: plain initialization calls, undone in `drop`.
        unsafe {
            // Fails harmlessly if this thread already uses another model.
            let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
                if com {
                    CoUninitialize();
                }
                return Err(format!("Media Foundation isn't available: {}", e.message()));
            }
            Ok(Self { com })
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: pairs with the successful calls in `start`.
        unsafe {
            let _ = MFShutdown();
            if self.com {
                CoUninitialize();
            }
        }
    }
}

/// An empty attribute store.
pub fn attributes(size: u32) -> windows::core::Result<IMFAttributes> {
    let mut attributes = None;
    // SAFETY: writes the new store into `attributes`.
    unsafe { MFCreateAttributes(&mut attributes, size)? };
    Ok(attributes.expect("created"))
}
