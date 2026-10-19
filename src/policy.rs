//! Settings an administrator can impose from outside snapr, read once at
//! start. Uploading can be turned off with:
//!
//! - the environment variable `SNAPR_DISABLE_UPLOAD`, set to anything but
//!   empty, `0`, `false`, `no` or `off`;
//! - on Windows, a `DisableUpload` value (a DWORD that isn't 0, or a string as
//!   above) under `SOFTWARE\Policies\snapr` or `SOFTWARE\snapr`, in
//!   `HKEY_LOCAL_MACHINE` or `HKEY_CURRENT_USER`.
//!
//! Any one of them turns uploading off; none can turn it back on, so a user
//! can't undo what the machine-wide key says.

use std::sync::OnceLock;

const ENV: &str = "SNAPR_DISABLE_UPLOAD";

/// What's shown where uploading would be.
pub const UPLOADS_OFF: &str = "Uploading has been turned off by your administrator";

/// Whether uploading has been turned off.
pub fn uploads_disabled() -> bool {
    static DISABLED: OnceLock<bool> = OnceLock::new();
    *DISABLED.get_or_init(|| {
        let disabled = std::env::var(ENV).is_ok_and(|v| truthy(&v)) || registry::disable_upload();
        if disabled {
            eprintln!("uploading is turned off by policy");
        }
        disabled
    })
}

fn truthy(value: &str) -> bool {
    let v = value.trim();
    !(v.is_empty() || ["0", "false", "no", "off"].iter().any(|f| v.eq_ignore_ascii_case(f)))
}

#[cfg(windows)]
mod registry {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
    };

    const KEYS: [&str; 2] = [r"SOFTWARE\Policies\snapr", r"SOFTWARE\snapr"];

    pub fn disable_upload() -> bool {
        [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER]
            .into_iter()
            .flat_map(|root| KEYS.map(|key| (root, key)))
            .any(|(root, key)| set(root, key, "DisableUpload"))
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    /// Whether `value` under `root\key` is a DWORD that isn't 0 or a string
    /// that means yes.
    fn set(root: HKEY, key: &str, value: &str) -> bool {
        let (key, value) = (wide(key), wide(value));
        let mut dword = 0u32;
        let mut len = 4u32;
        // SAFETY: the strings are NUL-terminated and outlive the call; `len`
        // is the size of `dword` in bytes.
        let err = unsafe {
            RegGetValueW(
                root,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&raw mut dword).cast(),
                &mut len,
            )
        };
        if err == ERROR_SUCCESS {
            return dword != 0;
        }
        let mut text = [0u16; 64];
        let mut len = size_of_val(&text) as u32;
        // SAFETY: as above, with `len` the size of `text` in bytes. A longer
        // string fails, which is fine: no word that means yes is that long.
        let err = unsafe {
            RegGetValueW(
                root,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                text.as_mut_ptr().cast(),
                &mut len,
            )
        };
        if err != ERROR_SUCCESS {
            return false;
        }
        let chars = (len as usize / 2).saturating_sub(1); // without the NUL
        super::truthy(&String::from_utf16_lossy(&text[..chars]))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use windows_sys::Win32::System::Registry::{REG_DWORD, REG_SZ, RegDeleteTreeW, RegSetKeyValueW};

        #[test]
        fn reads_dwords_and_strings() {
            let key = format!(r"SOFTWARE\snapr-test-{}", std::process::id());
            let put = |name: &str, kind, data: &[u8]| {
                let (k, n) = (wide(&key), wide(name));
                // SAFETY: the strings are NUL-terminated and `data` is as long as it says.
                let err = unsafe {
                    let (data, len) = (data.as_ptr().cast(), data.len() as u32);
                    RegSetKeyValueW(HKEY_CURRENT_USER, k.as_ptr(), n.as_ptr(), kind, data, len)
                };
                assert_eq!(err, ERROR_SUCCESS);
            };
            let text = |s: &str| wide(s).iter().flat_map(|c| c.to_le_bytes()).collect::<Vec<u8>>();
            put("One", REG_DWORD, &1u32.to_le_bytes());
            put("Zero", REG_DWORD, &0u32.to_le_bytes());
            put("Yes", REG_SZ, &text("true"));
            put("No", REG_SZ, &text("0"));
            let read = |name| set(HKEY_CURRENT_USER, &key, name);
            let got = [read("One"), read("Zero"), read("Yes"), read("No"), read("Missing")];
            // SAFETY: as above.
            unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, wide(&key).as_ptr()) };
            assert_eq!(got, [true, false, true, false, false]);
        }
    }
}

#[cfg(not(windows))]
mod registry {
    pub fn disable_upload() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::truthy;

    #[test]
    fn reads_yes_and_no() {
        for yes in ["1", "true", "YES", " on ", "anything"] {
            assert!(truthy(yes), "{yes:?}");
        }
        for no in ["", " ", "0", "false", "No", "OFF"] {
            assert!(!truthy(no), "{no:?}");
        }
    }
}
