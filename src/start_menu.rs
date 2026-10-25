//! snapr in the Start menu on Windows: a shortcut to snapr's own file in the
//! current user's Start menu programs. snapr has no installer to put one
//! there, so it adds it itself.

use std::path::{Path, PathBuf};

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
    IPersistFile, STGM_READ,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::core::{HSTRING, Interface};

fn shortcut() -> Result<PathBuf, String> {
    let roaming = dirs::data_dir().ok_or("couldn't find the Start menu folder")?;
    Ok(roaming.join(r"Microsoft\Windows\Start Menu\Programs\snapr.lnk"))
}

/// Adds snapr to the Start menu (for its file where it is now) or takes it
/// out.
pub fn set(enabled: bool) -> Result<(), String> {
    if enabled {
        return refresh();
    }
    match std::fs::remove_file(shortcut()?) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("couldn't take snapr out of the Start menu: {e}"))
        }
        _ => Ok(()),
    }
}

/// Adds the shortcut, or points it at snapr's file where it is now (in case
/// it was moved). One that already points there is left alone.
pub fn refresh() -> Result<(), String> {
    let exe = crate::update::exe()?;
    let lnk = shortcut()?;
    // SAFETY: COM is started for this call and stopped after it, and the
    // shell link is released before that.
    unsafe {
        // Fails harmlessly if this thread already uses another model.
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let result = write(exe, &lnk);
        if com {
            CoUninitialize();
        }
        result.map_err(|e| format!("couldn't add snapr to the Start menu: {}", e.message()))
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
    /// Adds snapr to the Start menu and takes it out again:
    /// `cargo test start_menu_round_trip -- --ignored`. Leaves it out.
    #[test]
    #[ignore]
    fn start_menu_round_trip() {
        let lnk = super::shortcut().unwrap();
        super::set(true).unwrap();
        let written = std::fs::metadata(&lnk).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        super::refresh().unwrap(); // already there, so left alone
        assert_eq!(std::fs::metadata(&lnk).unwrap().modified().unwrap(), written);
        super::set(false).unwrap();
        super::set(false).unwrap(); // already out
        assert!(!lnk.exists());
    }
}
