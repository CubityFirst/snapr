//! Upload secret keys, kept in the OS credential store (Windows Credential
//! Manager, macOS Keychain, Secret Service on Linux), not in the config file.

use keyring::{Entry, Error};

const SERVICE: &str = "snapr";

fn entry(id: &str) -> Result<Entry, String> {
    Entry::new(SERVICE, &format!("upload-{id}"))
        .map_err(|e| format!("credential store unavailable: {e}"))
}

pub fn get(id: &str) -> Option<String> {
    entry(id).ok()?.get_password().ok()
}

pub fn set(id: &str, secret: &str) -> Result<(), String> {
    entry(id)?
        .set_password(secret)
        .map_err(|e| format!("couldn't store the secret key: {e}"))
}

pub fn delete(id: &str) {
    if let Ok(e) = entry(id) {
        match e.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => {}
            Err(err) => eprintln!("couldn't remove a stored secret key: {err}"),
        }
    }
}

#[cfg(test)]
mod tests {
    /// Writes, reads and deletes a throwaway credential in the real store:
    /// `cargo test credential_store -- --ignored`.
    #[test]
    #[ignore]
    fn credential_store_round_trip() {
        let id = format!("selftest-{}", fastrand::u32(..));
        super::set(&id, "s3cr3t").unwrap();
        assert_eq!(super::get(&id).as_deref(), Some("s3cr3t"));
        super::delete(&id);
        assert_eq!(super::get(&id), None);
    }
}
