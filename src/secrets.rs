//! Upload secret keys, kept in the OS credential store (Windows Credential
//! Manager, macOS Keychain, Secret Service on Linux), not in the config file.
//!
//! Each is stored as `upload-<kind>-<id>` (e.g. `upload-s3-pt5uva7kmher`),
//! under the service `snapr`, or with `SNAPR_CONFIG_DIR` set a service of
//! that folder's own, so a second copy can never read or replace the keys of
//! the usual one. S3 keys stored before the kind was part of the name, as
//! `upload-<id>`, are moved over the first time they're read.

use std::ffi::OsStr;

use keyring::{Entry, Error};
use sha2::{Digest, Sha256};

use crate::settings::{Upload, UploadKind};

/// The credential store's service for the config folder `SNAPR_CONFIG_DIR`
/// points at, if it's set.
fn service_for(config_dir: Option<&OsStr>) -> String {
    match config_dir {
        None => "snapr".into(),
        Some(dir) => {
            let hash = Sha256::digest(dir.to_string_lossy().as_bytes());
            let short: String = hash[..6].iter().map(|b| format!("{b:02x}")).collect();
            format!("snapr-{short}")
        }
    }
}

fn service() -> String {
    service_for(std::env::var_os("SNAPR_CONFIG_DIR").as_deref())
}

/// The name a destination's key is stored under.
fn name(kind: UploadKind, id: &str) -> String {
    format!("upload-{}-{id}", kind.slug())
}

/// Where an older snapr kept the key, if anywhere: S3 keys were stored by
/// id alone.
fn legacy_name(kind: UploadKind, id: &str) -> Option<String> {
    (kind == UploadKind::S3).then(|| format!("upload-{id}"))
}

/// A key found by [`find`], and the old name it has to be moved from.
#[derive(Debug, PartialEq)]
struct Found {
    secret: String,
    move_from: Option<String>,
}

/// Looks a destination's key up with `stored` (name → key): under its own
/// name, or failing that under the legacy one, to be moved.
fn find(kind: UploadKind, id: &str, stored: impl Fn(&str) -> Option<String>) -> Option<Found> {
    if let Some(secret) = stored(&name(kind, id)) {
        return Some(Found {
            secret,
            move_from: None,
        });
    }
    let legacy = legacy_name(kind, id)?;
    Some(Found {
        secret: stored(&legacy)?,
        move_from: Some(legacy),
    })
}

fn entry(name: &str) -> Result<Entry, String> {
    Entry::new(&service(), name).map_err(|e| format!("credential store unavailable: {e}"))
}

fn read(name: &str) -> Option<String> {
    entry(name).ok()?.get_password().ok()
}

fn write(name: &str, secret: &str) -> Result<(), String> {
    entry(name)?
        .set_password(secret)
        .map_err(|e| format!("couldn't store the secret key: {e}"))
}

fn remove(name: &str) {
    if let Ok(e) = entry(name) {
        match e.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => {}
            Err(err) => eprintln!("couldn't remove a stored secret key: {err}"),
        }
    }
}

pub fn get(upload: &Upload) -> Option<String> {
    let found = find(upload.kind, &upload.id, read)?;
    if let Some(old) = &found.move_from
        && write(&name(upload.kind, &upload.id), &found.secret).is_ok()
    {
        remove(old);
    }
    Some(found.secret)
}

pub fn set(upload: &Upload, secret: &str) -> Result<(), String> {
    write(&name(upload.kind, &upload.id), secret)?;
    // A legacy copy would only be stale now.
    if let Some(old) = legacy_name(upload.kind, &upload.id) {
        remove(&old);
    }
    Ok(())
}

pub fn delete(upload: &Upload) {
    remove(&name(upload.kind, &upload.id));
    if let Some(old) = legacy_name(upload.kind, &upload.id) {
        remove(&old);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn names_carry_the_kind() {
        assert_eq!(name(UploadKind::S3, "pt5uva7kmher"), "upload-s3-pt5uva7kmher");
        assert_eq!(name(UploadKind::Pomf, "x"), "upload-pomf-x");
    }

    #[test]
    fn a_config_dir_gets_its_own_service() {
        assert_eq!(service_for(None), "snapr");
        let a = service_for(Some(OsStr::new("C:\\snapr-test\\config")));
        let b = service_for(Some(OsStr::new("C:\\other\\config")));
        assert!(a.starts_with("snapr-") && a != "snapr", "{a}");
        assert_ne!(a, b);
        // The same folder always finds the same keys again.
        assert_eq!(a, service_for(Some(OsStr::new("C:\\snapr-test\\config"))));
    }

    #[test]
    fn legacy_s3_keys_are_found_and_moved() {
        let store = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
        };
        let lookup = |s: &HashMap<String, String>| {
            let s = s.clone();
            move |n: &str| s.get(n).cloned()
        };

        // Only the old name: found, to be moved.
        let old = store(&[("upload-abc", "r2")]);
        assert_eq!(
            find(UploadKind::S3, "abc", lookup(&old)),
            Some(Found {
                secret: "r2".into(),
                move_from: Some("upload-abc".into()),
            })
        );
        // Both: the new name wins and nothing moves.
        let both = store(&[("upload-abc", "stale"), ("upload-s3-abc", "r2")]);
        assert_eq!(
            find(UploadKind::S3, "abc", lookup(&both)),
            Some(Found {
                secret: "r2".into(),
                move_from: None,
            })
        );
        // Other kinds never had the old name, so never take an S3 key.
        assert_eq!(find(UploadKind::Pomf, "abc", lookup(&old)), None);
        assert_eq!(find(UploadKind::S3, "abc", lookup(&store(&[]))), None);
    }

    /// Writes, reads and deletes a throwaway credential in the real store,
    /// moving it from the legacy name on the way:
    /// `cargo test credential_store -- --ignored`.
    #[test]
    #[ignore]
    fn credential_store_round_trip() {
        let upload = Upload {
            id: format!("selftest-{}", fastrand::u32(..)),
            ..Upload::default()
        };
        write(&legacy_name(upload.kind, &upload.id).unwrap(), "old").unwrap();
        assert_eq!(get(&upload).as_deref(), Some("old"));
        assert_eq!(read(&legacy_name(upload.kind, &upload.id).unwrap()), None);
        set(&upload, "s3cr3t").unwrap();
        assert_eq!(get(&upload).as_deref(), Some("s3cr3t"));
        delete(&upload);
        assert_eq!(get(&upload), None);
    }
}
