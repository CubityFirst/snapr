//! snapr's shortcuts on Windows: one to its own file in the current user's
//! Start menu programs, and one on their desktop. snapr has no installer to
//! put them there, so it adds them itself.

use std::path::{Path, PathBuf};

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
    IPersistFile, STGM_READ,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::core::{HSTRING, Interface};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    StartMenu,
    Desktop,
}

impl Shortcut {
    fn path(self) -> Result<PathBuf, String> {
        match self {
            Shortcut::StartMenu => {
                let roaming = dirs::data_dir().ok_or("couldn't find the Start menu folder")?;
                Ok(roaming.join(r"Microsoft\Windows\Start Menu\Programs\snapr.lnk"))
            }
            // The known folder, so a desktop OneDrive keeps is found too.
            Shortcut::Desktop => {
                let desktop = dirs::desktop_dir().ok_or("couldn't find your desktop")?;
                Ok(desktop.join("snapr.lnk"))
            }
        }
    }

    fn adding(self) -> &'static str {
        match self {
            Shortcut::StartMenu => "couldn't add snapr to the Start menu",
            Shortcut::Desktop => "couldn't put a shortcut to snapr on the desktop",
        }
    }

    fn removing(self) -> &'static str {
        match self {
            Shortcut::StartMenu => "couldn't take snapr out of the Start menu",
            Shortcut::Desktop => "couldn't take snapr's shortcut off the desktop",
        }
    }
}

/// Whether the shortcut is there (wherever it points).
pub fn exists(which: Shortcut) -> bool {
    which.path().is_ok_and(|lnk| lnk.is_file())
}

/// Adds the shortcut (for snapr's file where it is now) or takes it out.
pub fn set(which: Shortcut, enabled: bool) -> Result<(), String> {
    if enabled {
        return refresh(which);
    }
    match std::fs::remove_file(which.path()?) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("{}: {e}", which.removing()))
        }
        _ => Ok(()),
    }
}

/// Adds the shortcut, or points it at snapr's file where it is now (in case
/// it was moved). One that already points there is left alone.
pub fn refresh(which: Shortcut) -> Result<(), String> {
    let exe = crate::update::exe()?;
    let lnk = which.path()?;
    // SAFETY: COM is started for this call and stopped after it, and the
    // shell link is released before that.
    unsafe {
        // Fails harmlessly if this thread already uses another model.
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let result = write(exe, &lnk);
        if com {
            CoUninitialize();
        }
        result.map_err(|e| format!("{}: {}", which.adding(), e.message()))
    }
}

unsafe fn write(exe: &Path, lnk: &Path) -> windows::core::Result<()> {
    // SAFETY: the caller started COM; the buffers outlive the calls.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        let file: IPersistFile = link.cast()?;
        let lnk = HSTRING::from(lnk);
        if file.Load(&lnk, STGM_READ).is_ok() {
            let mut target = [0u16; 32768];
            if link.GetPath(&mut target, std::ptr::null_mut(), 0).is_ok() {
                let len = target.iter().position(|&c| c == 0).unwrap_or(target.len());
                if Path::new(&String::from_utf16_lossy(&target[..len])) == exe {
                    return Ok(());
                }
            }
        }
        link.SetPath(&HSTRING::from(exe))?;
        file.Save(&lnk, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Adds each shortcut and takes it out again:
    /// `cargo test shortcut_round_trip -- --ignored`. Leaves them out.
    #[test]
    #[ignore]
    fn shortcut_round_trip() {
        for which in [Shortcut::StartMenu, Shortcut::Desktop] {
            let lnk = which.path().unwrap();
            set(which, true).unwrap();
            assert!(exists(which));
            let written = std::fs::metadata(&lnk).unwrap().modified().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(50));
            refresh(which).unwrap(); // already there, so left alone
            assert_eq!(std::fs::metadata(&lnk).unwrap().modified().unwrap(), written);
            set(which, false).unwrap();
            set(which, false).unwrap(); // already out
            assert!(!exists(which));
        }
    }
}
