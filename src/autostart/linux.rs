//! Starting snapr when you sign in on Linux: an XDG autostart entry,
//! `~/.config/autostart/snapr.desktop`, naming snapr's own file. Desktops
//! start it in a unit named after the file (e.g. `app-gnome-snapr-….scope`),
//! so the portal still knows it as `snapr`.

use std::path::{Path, PathBuf};

fn entry() -> Result<PathBuf, String> {
    let dir = dirs::config_dir().ok_or("couldn't find your config folder")?;
    Ok(dir.join("autostart").join("snapr.desktop"))
}

/// Whether snapr starts at sign-in: the entry is there and the desktop's
/// startup settings (e.g. GNOME Settings' Apps page, which can make one
/// itself) haven't turned it off. The entry, not the config, is what counts.
pub fn enabled() -> bool {
    entry()
        .and_then(|p| std::fs::read_to_string(p).map_err(|e| e.to_string()))
        .is_ok_and(|e| !disabled(&e))
}

/// Turns starting at sign-in on (for snapr's file where it is now) or off.
pub fn set(enabled: bool) -> Result<(), String> {
    let path = entry()?;
    if !enabled {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("couldn't stop snapr starting when you sign in: {e}")),
        };
    }
    // A fresh file, which also undoes a desktop's "Hidden" or "disabled".
    let contents = desktop_entry(&exec()?);
    crate::atomic::write(&path, contents)
        .map_err(|e| format!("couldn't make snapr start when you sign in: {e}"))
}

/// Points starting at sign-in at snapr's file where it is now (in case it
/// was moved), leaving it off if the desktop's startup settings removed or
/// turned off the entry.
pub fn refresh() -> Result<(), String> {
    let path = entry()?;
    let Ok(old) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    if disabled(&old) {
        return Ok(());
    }
    let new = with_exec(&old, &exec()?);
    if new == old {
        return Ok(());
    }
    crate::atomic::write(&path, new)
        .map_err(|e| format!("couldn't make snapr start when you sign in: {e}"))
}

fn exec() -> Result<String, String> {
    Ok(quote(crate::update::exe()?))
}

fn desktop_entry(exec: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=snapr\n\
         Comment=Region screenshots from a hotkey\n\
         Exec={exec}\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n"
    )
}

/// Turned off by the desktop's startup settings without removing the file.
fn disabled(entry: &str) -> bool {
    entry.lines().map(str::trim).any(|l| {
        l.eq_ignore_ascii_case("Hidden=true") || l.eq_ignore_ascii_case("X-GNOME-Autostart-enabled=false")
    })
}

/// `entry` with its `Exec` line (in `[Desktop Entry]`) set to `exec`,
/// keeping everything else as the user or desktop left it.
fn with_exec(entry: &str, exec: &str) -> String {
    let mut out = String::with_capacity(entry.len() + exec.len());
    let mut main = false;
    for line in entry.lines() {
        if line.starts_with('[') {
            main = line.trim() == "[Desktop Entry]";
        }
        if main && line.trim_start().starts_with("Exec=") {
            out.push_str("Exec=");
            out.push_str(exec);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// `path` as a desktop entry's `Exec` value: quoted, with `"`, `` ` ``, `$`
/// and `\` escaped inside the quotes, then backslashes doubled again for the
/// string itself, and `%` (a field code) as `%%`.
fn quote(path: &Path) -> String {
    let mut arg = String::from("\"");
    for c in path.to_string_lossy().chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            arg.push('\\');
        }
        arg.push(c);
    }
    arg.push('"');
    arg.replace('\\', "\\\\").replace('%', "%%")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Turns starting at sign-in on and off again, checking the entry:
    /// `cargo test autostart_round_trip -- --ignored`. Leaves it off.
    #[test]
    #[ignore]
    fn autostart_round_trip() {
        set(true).unwrap();
        let text = std::fs::read_to_string(entry().unwrap()).unwrap();
        assert!(enabled() && text.contains("Exec=\""), "{text}");
        refresh().unwrap();
        assert_eq!(std::fs::read_to_string(entry().unwrap()).unwrap(), text);
        set(false).unwrap();
        set(false).unwrap(); // already off
        assert!(!enabled() && !entry().unwrap().exists());
    }

    #[test]
    fn quotes_paths() {
        assert_eq!(quote(Path::new("/home/a/bin/snapr")), "\"/home/a/bin/snapr\"");
        assert_eq!(
            quote(Path::new("/home/a/My Apps/100% $nap\"r")),
            "\"/home/a/My Apps/100%% \\\\$nap\\\\\"r\""
        );
    }

    #[test]
    fn rewrites_only_the_exec_line() {
        let old = "[Desktop Entry]\nName=snapr\nExec=\"/old/snapr\"\nX-KDE-Thing=1\n\
                   [Desktop Action new]\nExec=other\n";
        assert_eq!(
            with_exec(old, "\"/new/snapr\""),
            "[Desktop Entry]\nName=snapr\nExec=\"/new/snapr\"\nX-KDE-Thing=1\n\
             [Desktop Action new]\nExec=other\n"
        );
    }

    #[test]
    fn sees_entries_the_desktop_turned_off() {
        assert!(!disabled(&desktop_entry("snapr")));
        assert!(disabled("[Desktop Entry]\nExec=snapr\nHidden=true\n"));
        assert!(disabled("[Desktop Entry]\nExec=snapr\nX-GNOME-Autostart-enabled=false\n"));
    }
}
