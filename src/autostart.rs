//! Starting snapr when you sign in to Windows: a value under the current
//! user's `Run` key naming snapr's own file.

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RegDeleteKeyValueW, RegSetKeyValueW,
};

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Where Task Manager's Startup apps page keeps what it has turned off.
const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const NAME: &str = "snapr";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// Turns starting at sign-in on (for snapr's file where it is now) or off.
pub fn set(enabled: bool) -> Result<(), String> {
    let (run, approved, name) = (wide(RUN), wide(APPROVED), wide(NAME));
    if !enabled {
        // SAFETY: the strings are NUL-terminated and outlive the call.
        let err = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, run.as_ptr(), name.as_ptr()) };
        return match err {
            ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
            e => Err(format!("couldn't stop snapr starting with Windows (error {e})")),
        };
    }
    write_run(&run, &name)?;
    // Undo a "Disabled" from Task Manager, which would otherwise win.
    // SAFETY: as above.
    unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, approved.as_ptr(), name.as_ptr()) };
    Ok(())
}

/// Points starting at sign-in at snapr's file where it is now (in case it
/// was moved), leaving it off if Task Manager turned it off.
pub fn refresh() -> Result<(), String> {
    write_run(&wide(RUN), &wide(NAME))
}

fn write_run(run: &[u16], name: &[u16]) -> Result<(), String> {
    let exe = crate::update::exe()?;
    let command = wide(&format!("\"{}\"", exe.display()));
    // SAFETY: the strings are NUL-terminated and outlive the call; the
    // data's length is in bytes, NUL included.
    let err = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            run.as_ptr(),
            name.as_ptr(),
            REG_SZ,
            command.as_ptr().cast(),
            (command.len() * 2) as u32,
        )
    };
    if err != ERROR_SUCCESS {
        return Err(format!("couldn't make snapr start with Windows (error {err})"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// Turns starting with Windows on and off again, checking the Run key:
    /// `cargo test autostart_round_trip -- --ignored`. Leaves it off.
    #[test]
    #[ignore]
    fn autostart_round_trip() {
        let query = || {
            std::process::Command::new("reg")
                .args(["query", &format!(r"HKCU\{}", super::RUN), "/v", super::NAME])
                .output()
                .unwrap()
        };
        super::set(true).unwrap();
        let out = String::from_utf8_lossy(&query().stdout).into_owned();
        assert!(out.contains("REG_SZ") && out.contains(".exe\""), "{out}");
        super::set(false).unwrap();
        super::set(false).unwrap(); // already off
        assert!(!query().status.success());
    }
}
