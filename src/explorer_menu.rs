//! "Upload with Snapr" on File Explorer's right-click menu (Windows): a verb
//! for every file type under the current user's classes, which runs
//! `snapr --upload <file>`. On Windows 11 it's under "Show more options".

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RegDeleteTreeW, RegSetKeyValueW,
};

const VERB: &str = r"Software\Classes\*\shell\snapr.upload";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Adds the menu item (for snapr's file where it is now) or takes it out.
pub fn set(enabled: bool) -> Result<(), String> {
    if enabled {
        return refresh();
    }
    let key = wide(VERB);
    // SAFETY: the string is NUL-terminated and outlives the call.
    let err = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, key.as_ptr()) };
    if err == ERROR_SUCCESS || err == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(format!("couldn't take Upload with Snapr off the menu (error {err})"))
    }
}

/// Adds the menu item, or points it at snapr's file where it is now (in
/// case it was moved).
pub fn refresh() -> Result<(), String> {
    let exe = crate::update::exe()?.display().to_string();
    let command = format!("{VERB}\\command");
    let values = [
        (VERB, None, "Upload with Snapr".to_string()),
        (VERB, Some("Icon"), exe.clone()),
        (&command, None, format!("\"{exe}\" --upload \"%1\"")),
    ];
    for (key, name, value) in values {
        let (key, value) = (wide(key), wide(&value));
        let name = name.map(wide);
        // SAFETY: the strings are NUL-terminated and outlive the call; the
        // length is the value's in bytes. The key is made if needed.
        let err = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()),
                REG_SZ,
                value.as_ptr().cast(),
                (value.len() * 2) as u32,
            )
        };
        if err != ERROR_SUCCESS {
            return Err(format!("couldn't add Upload with Snapr to the menu (error {err})"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// Adds the menu item and takes it out again:
    /// `cargo test explorer_menu_round_trip -- --ignored`. Leaves it out.
    #[test]
    #[ignore]
    fn explorer_menu_round_trip() {
        let query = |key: &str| {
            std::process::Command::new("reg")
                .args(["query", &format!(r"HKCU\{key}"), "/ve"])
                .output()
                .unwrap()
        };
        super::set(true).unwrap();
        let command = query(&format!(r"{}\command", super::VERB));
        assert!(String::from_utf8_lossy(&command.stdout).contains("--upload \"%1\""));
        super::set(false).unwrap();
        super::set(false).unwrap(); // already out
        assert!(!query(super::VERB).status.success());
    }
}
