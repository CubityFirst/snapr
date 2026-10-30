//! snapr's place on Windows, without an installer: it can move itself to the
//! current user's Programs folder (no administrator rights needed), lists
//! itself in Settings > Apps, and uninstalls itself from there
//! (`snapr --uninstall`).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_DWORD, REG_SZ, RegDeleteTreeW, RegSetKeyValueW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    IDOK, IDYES, MB_DEFBUTTON2, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK,
    MB_OKCANCEL, MB_YESNO, MESSAGEBOX_STYLE, MessageBoxW,
};

use crate::settings::Settings;

/// Settings > Apps' entry for snapr.
const UNINSTALL: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\snapr";
/// Longest an uninstall waits for a running snapr to quit.
const QUIT_WAIT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// In the Programs folder.
    Home,
    /// Wherever it was downloaded.
    Elsewhere,
    /// Built from source, in a Cargo `target` folder: it's left where it is
    /// and out of Settings > Apps.
    Development,
}

/// Where snapr's own file is, worked out once.
pub fn place() -> Place {
    static PLACE: OnceLock<Place> = OnceLock::new();
    *PLACE.get_or_init(|| {
        let Ok(exe) = crate::update::exe() else {
            return Place::Development;
        };
        let built = exe.ancestors().any(|dir| {
            dir.file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("target"))
                && dir.parent().is_some_and(|p| p.join("Cargo.toml").is_file())
        });
        if built || cfg!(debug_assertions) {
            Place::Development
        } else if home().is_some_and(|home| same(exe, &home.join("snapr.exe"))) {
            Place::Home
        } else {
            Place::Elsewhere
        }
    })
}

/// `%LOCALAPPDATA%\Programs\snapr`.
pub fn home() -> Option<PathBuf> {
    Some(dirs::data_local_dir()?.join("Programs").join("snapr"))
}

fn same(a: &Path, b: &Path) -> bool {
    a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
}

/// Copies snapr's file into the Programs folder, ready for `relaunch`.
pub fn copy_home() -> Result<PathBuf, String> {
    let exe = crate::update::exe()?;
    let home = home().ok_or("couldn't find your Programs folder")?;
    let fail = |e: std::io::Error| format!("couldn't copy snapr to {}: {e}", home.display());
    std::fs::create_dir_all(&home).map_err(fail)?;
    let installed = home.join("snapr.exe");
    // Copied beside it first, so a copy that fails part-way never replaces
    // one that's there; the update check clears up a `.new` left behind.
    let new = home.join("snapr.exe.new");
    std::fs::copy(exe, &new)
        .and_then(|_| std::fs::rename(&new, &installed))
        .map_err(|e| {
            let _ = std::fs::remove_file(&new);
            fail(e)
        })?;
    Ok(installed)
}

/// Starts the copy `copy_home` made, which removes this one once it has
/// quit, and opens its window (on Settings if `settings`). Call once this
/// one has let go of the single-instance lock.
pub fn relaunch(installed: &Path, settings: bool) -> Result<(), String> {
    let mut command = std::process::Command::new(installed);
    command.arg("--moved-from").arg(crate::update::exe()?);
    if settings {
        command.arg("--settings");
    }
    let started = command.spawn();
    // Should it not start, or stop straight away with an error (refused to
    // run, say), this copy starts again rather than leave snapr not running.
    let failed = match started {
        Err(_) => true,
        Ok(mut child) => {
            let start = Instant::now();
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => break !status.success(),
                    Ok(None) if start.elapsed() < Duration::from_secs(3) => {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    _ => break false,
                }
            }
        }
    };
    if failed {
        std::process::Command::new(crate::update::exe()?)
            .arg("--settings")
            .spawn()
            .map_err(|e| format!("couldn't start snapr again: {e}"))?;
        return Err(format!("couldn't start snapr from {}", installed.display()));
    }
    Ok(())
}

/// Removes the copy snapr was moved from, once it's quit.
pub fn remove_moved(old: PathBuf) {
    let ours = crate::update::exe().is_ok_and(|exe| same(exe, &old));
    if ours
        || !old
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
    {
        return;
    }
    std::thread::spawn(move || {
        let start = Instant::now();
        loop {
            // With it, the copies an update there moved aside, the
            // one that ran this move among them.
            let mut left = leftovers(&old);
            left.push(old.clone());
            left.retain(|file| std::fs::remove_file(file).is_err() && file.exists());
            if left.is_empty() || start.elapsed() > Duration::from_secs(10) {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    });
}

/// The `snapr.exe.old…` files an update put beside `exe`.
fn leftovers(exe: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(name)) = (exe.parent(), exe.file_name()) else {
        return Vec::new();
    };
    let old = format!("{}.old", name.to_string_lossy());
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&old))
        .map(|entry| entry.path())
        .collect()
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// Lists snapr in Settings > Apps, for its file where it is now (in case it
/// was moved), so it can be uninstalled from there.
pub fn register() -> Result<(), String> {
    if place() == Place::Development {
        return Ok(());
    }
    let exe = crate::update::exe()?;
    let dir = exe.parent().unwrap_or(exe);
    let size_kb = std::fs::metadata(exe).map_or(0, |m| (m.len() / 1024) as u32);
    let text = [
        ("DisplayName", "snapr".to_string()),
        ("DisplayVersion", env!("CARGO_PKG_VERSION").to_string()),
        ("Publisher", "Michael Burr".to_string()),
        ("DisplayIcon", exe.display().to_string()),
        ("InstallLocation", dir.display().to_string()),
        (
            "UninstallString",
            format!("\"{}\" --uninstall", exe.display()),
        ),
        ("URLInfoAbout", env!("CARGO_PKG_REPOSITORY").to_string()),
    ];
    let numbers = [("NoModify", 1), ("NoRepair", 1), ("EstimatedSize", size_kb)];
    let key = wide(UNINSTALL);
    let set = |name: &str, kind, data: *const u16, bytes: usize| {
        let name = wide(name);
        // SAFETY: the strings are NUL-terminated and, like the data, outlive
        // the call; `bytes` is the data's length. The key is made if needed.
        let err = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                kind,
                data.cast(),
                bytes as u32,
            )
        };
        if err == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!(
                "couldn't list snapr in Settings > Apps (error {err})"
            ))
        }
    };
    for (name, value) in text {
        let value = wide(&value);
        set(name, REG_SZ, value.as_ptr(), value.len() * 2)?;
    }
    for (name, value) in numbers {
        set(name, REG_DWORD, (&raw const value).cast(), 4)?;
    }
    Ok(())
}

/// Takes snapr out of Settings > Apps.
fn unregister() {
    let key = wide(UNINSTALL);
    // SAFETY: the string is NUL-terminated and outlives the call.
    let err = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, key.as_ptr()) };
    if err != ERROR_SUCCESS && err != ERROR_FILE_NOT_FOUND {
        eprintln!("couldn't take snapr out of Settings > Apps (error {err})");
    }
}

fn message(text: &str, style: MESSAGEBOX_STYLE) -> i32 {
    let (text, title) = (wide(text), wide("Uninstall snapr"));
    // SAFETY: the strings are NUL-terminated and outlive the call.
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), style) }
}

/// `snapr --uninstall`, which Settings > Apps runs: asks first, quits a
/// running snapr, takes it out of the Start menu, startup and Settings >
/// Apps, deletes its settings if asked to, then its own file. Screenshots
/// are left alone. Returns the exit code.
pub fn uninstall() -> i32 {
    let ok = message(
        "Uninstall snapr?\n\nIt's taken out of the Start menu and the right-click menu and stops \
         starting with Windows. Your screenshots and recordings stay where they are.",
        MB_OKCANCEL | MB_ICONQUESTION,
    );
    if ok != IDOK {
        return 1;
    }
    let wipe = message(
        "Also delete snapr's settings, its list of recent captures and the upload passwords \
         it saved?\n\nKeep them to pick up where you left off if you install snapr again.",
        MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
    ) == IDYES;
    if !quit_running() {
        message(
            "snapr is still running. Quit it from its tray icon, then try again.",
            MB_OK | MB_ICONERROR,
        );
        return 1;
    }
    for result in [
        crate::autostart::set(false),
        crate::start_menu::set(false),
        crate::explorer_menu::set(false),
    ] {
        if let Err(e) = result {
            eprintln!("{e}");
        }
    }
    unregister();
    if wipe {
        delete_settings();
    }
    let Ok(exe) = crate::update::exe() else {
        message("snapr has been uninstalled.", MB_OK | MB_ICONINFORMATION);
        return 0;
    };
    // Said before the file goes: it can't while this message is up.
    match deleter(exe) {
        Some(mut deleter) => {
            message("snapr has been uninstalled.", MB_OK | MB_ICONINFORMATION);
            if let Err(e) = deleter.spawn() {
                eprintln!("couldn't delete {}: {e}", exe.display());
            }
        }
        None => {
            let done = format!(
                "snapr has been uninstalled. You can delete {} now.",
                exe.display()
            );
            message(&done, MB_OK | MB_ICONINFORMATION);
        }
    }
    0
}

/// Asks a running snapr to quit and waits until it has. Returns whether
/// none is running.
fn quit_running() -> bool {
    let Some(lock) = Settings::config_dir().map(|dir| dir.join("snapr.lock")) else {
        return true;
    };
    let free = || {
        std::fs::OpenOptions::new()
            .write(true)
            .open(&lock)
            .map_or(true, |file| file.try_lock().is_ok())
    };
    if free() {
        return true;
    }
    crate::tray::quit_running_instance();
    let start = Instant::now();
    while start.elapsed() < QUIT_WAIT {
        std::thread::sleep(Duration::from_millis(250));
        if free() {
            return true;
        }
    }
    false
}

/// The config folder, the cache, and the upload keys in Credential Manager.
fn delete_settings() {
    for upload in &Settings::load().0.uploads {
        crate::secrets::delete(upload);
    }
    // A folder of its own given with SNAPR_CONFIG_DIR is the user's to clear,
    // and the usual copy of snapr shares the cache.
    if std::env::var_os("SNAPR_CONFIG_DIR").is_some() {
        return;
    }
    if let Some(dir) = Settings::config_dir() {
        let _ = std::fs::remove_dir_all(dir);
    }
    if let Some(cache) = dirs::cache_dir() {
        let _ = std::fs::remove_dir_all(cache.join("snapr"));
    }
}

/// A hidden command that deletes snapr's file once this program has exited,
/// as Windows won't delete a running one, and its folder too if that's the
/// Programs one. None if its path can't be given to cmd.
fn deleter(exe: &Path) -> Option<std::process::Command> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let quoted = |p: &Path| {
        let p = p.display().to_string();
        // cmd would read these as its own.
        (!p.contains(['"', '%'])).then(|| format!("\"{p}\""))
    };
    let delete = match exe.parent().filter(|_| place() == Place::Home) {
        Some(folder) => format!("rmdir /s /q {}", quoted(folder)?),
        None => format!("del /f /q {}", quoted(exe)?),
    };
    // A couple of seconds' wait (ping, as there's no console for timeout)
    // for this program to exit; cmd /c drops the outer quotes. Run from
    // elsewhere, as cmd can't remove the folder it's in.
    let mut command = std::process::Command::new("cmd");
    command
        .raw_arg(format!("/d /c \"ping -n 3 127.0.0.1 >nul & {delete}\""))
        .current_dir(std::env::temp_dir())
        .creation_flags(CREATE_NO_WINDOW);
    Some(command)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_test_build_is_development() {
        assert_eq!(super::place(), super::Place::Development);
    }
}
