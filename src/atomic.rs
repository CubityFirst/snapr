//! Crash-safe file saves: the new contents go to a temporary file beside the
//! old, which is replaced only once they're all on disk. A crash or power cut
//! mid-save leaves the old file as it was, never an empty or half-written one.

use std::io::Write;
use std::path::Path;

/// Writes `contents` to `path`, creating its folder if need be.
pub fn write(path: &Path, contents: impl AsRef<[u8]>) -> std::io::Result<()> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty());
    if let Some(dir) = dir {
        std::fs::create_dir_all(dir)?;
    }
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let written = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(contents.as_ref())?;
        file.sync_all()
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // A virus scanner or backup can hold the old file open for a moment,
    // and Windows won't replace it until it's let go.
    let mut tries = 0;
    loop {
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && tries < 5 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        }
    }
}

/// Keeps a copy of a file that couldn't be read, as `<name>.broken` beside
/// it, before it's saved over with defaults. Returns the copy's name.
pub fn keep_broken(path: &Path) -> Option<String> {
    let name = format!("{}.broken", path.file_name()?.to_string_lossy());
    std::fs::copy(path, path.with_file_name(&name)).ok()?;
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_the_file_and_leaves_nothing_behind() {
        let dir = std::env::temp_dir().join(format!("snapr-atomic-{}", fastrand::u64(..)));
        let path = dir.join("nested").join("config.toml");
        write(&path, "one").unwrap();
        write(&path, "two").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
        let files: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(files, ["config.toml"]);

        assert_eq!(keep_broken(&path).as_deref(), Some("config.toml.broken"));
        assert_eq!(std::fs::read_to_string(dir.join("nested/config.toml.broken")).unwrap(), "two");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
