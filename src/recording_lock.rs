//! Lets `snapr --record` stop the recording in progress, as pressing the
//! record hotkey again does. Whoever records (a `--record`, or the running
//! snapr) holds a lock on a file in the config folder; a `--record` that
//! finds it held leaves a stop request beside it instead, which the
//! recorder takes while it records. The lock goes with the process, so one
//! that crashed doesn't leave it held.

use std::fs::{File, OpenOptions};
use std::path::Path;

use crate::settings::Settings;

const LOCK: &str = "recording.lock";
const STOP: &str = "recording.stop";

pub enum Claim {
    /// Nobody else is recording; keep this for as long as this one may.
    Held(File),
    /// Another process is recording (or picking a region to).
    Busy,
    /// There's no telling: no config folder, or no file locking.
    Unknown,
}

/// Claims the right to record, dropping any stop request left over.
pub fn claim() -> Claim {
    Settings::config_dir().map_or(Claim::Unknown, |dir| claim_in(&dir))
}

/// Asks the process holding the lock to stop its recording.
pub fn request_stop() -> Result<(), String> {
    request_stop_in(&Settings::config_dir().ok_or("no config folder")?)
}

/// Whether a stop was requested since the last call, taking the request.
pub fn stop_requested() -> bool {
    Settings::config_dir().is_some_and(|dir| stop_requested_in(&dir))
}

fn claim_in(dir: &Path) -> Claim {
    let _ = std::fs::create_dir_all(dir);
    let Ok(file) = OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(LOCK)) else {
        return Claim::Unknown;
    };
    match file.try_lock() {
        Ok(()) => {
            stop_requested_in(dir);
            Claim::Held(file)
        }
        Err(std::fs::TryLockError::WouldBlock) => Claim::Busy,
        Err(_) => Claim::Unknown,
    }
}

fn request_stop_in(dir: &Path) -> Result<(), String> {
    File::create(dir.join(STOP)).map(drop).map_err(|e| format!("couldn't ask it to stop: {e}"))
}

fn stop_requested_in(dir: &Path) -> bool {
    std::fs::remove_file(dir.join(STOP)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_claim_is_busy_and_can_ask_the_first_to_stop() {
        let dir = std::env::temp_dir().join(format!("snapr-recording-lock-{}", std::process::id()));
        let Claim::Held(first) = claim_in(&dir) else {
            panic!("the first claim should hold the lock");
        };
        assert!(matches!(claim_in(&dir), Claim::Busy));
        assert!(!stop_requested_in(&dir));
        request_stop_in(&dir).unwrap();
        assert!(stop_requested_in(&dir));
        assert!(!stop_requested_in(&dir), "a request is taken once");
        // A request left over when the recorder quit doesn't stop the next.
        request_stop_in(&dir).unwrap();
        drop(first);
        assert!(matches!(claim_in(&dir), Claim::Held(_)));
        assert!(!stop_requested_in(&dir));
        let _ = std::fs::remove_dir_all(dir);
    }
}
