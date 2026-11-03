//! Copies that outlive `--once` and `--record`. On Linux the app that copied
//! serves the clipboard itself, so its contents go when it quits (unless a
//! clipboard manager takes them over). snapr quits straight after a one-off
//! capture, so then the last copy is handed to a copy of snapr started to
//! hold it, `snapr --hold-clipboard`, which quits once something else is
//! copied.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use image::RgbaImage;

/// The argument that starts a holder.
pub const ARG: &str = "--hold-clipboard";

static ENABLED: AtomicBool = AtomicBool::new(false);
/// The last copy, once enabled.
static LAST: Mutex<Option<Clip>> = Mutex::new(None);

pub enum Clip {
    Image(RgbaImage),
    Text(String),
    File(PathBuf),
}

/// Keeps copies for `hand_off`: snapr quits after this capture.
pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

/// Notes a copy just made, if enabled.
pub fn remember(clip: impl FnOnce() -> Clip) {
    if ENABLED.load(Ordering::Relaxed) {
        *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some(clip());
    }
}

/// Starts a holder for the last copy, if there was one, left to run on its
/// own. Called as snapr quits, after the last copy.
pub fn hand_off() {
    let Some(clip) = LAST.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return;
    };
    if let Err(e) = hold(clip) {
        eprintln!("the clipboard won't outlive snapr: {e}");
    }
}

fn hold(clip: Clip) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new(std::env::current_exe()?);
    command.arg(ARG);
    match &clip {
        Clip::Image(img) => command.args(["image", &img.width().to_string(), &img.height().to_string()]),
        Clip::Text(_) => command.arg("text"),
        Clip::File(path) => command.arg("file").arg(path),
    };
    // Its own process group, so a Ctrl+C meant for `snapr --once` in a
    // terminal doesn't take the copy with it.
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .process_group(0)
        .spawn()?;
    let mut stdin = child.stdin.take().expect("piped");
    match &clip {
        Clip::Image(img) => stdin.write_all(img.as_raw()),
        Clip::Text(text) => stdin.write_all(text.as_bytes()),
        Clip::File(_) => Ok(()),
    }
}

/// `snapr --hold-clipboard image <width> <height>` (RGBA on stdin), `text`
/// (on stdin) or `file <path>`: puts it on the clipboard and serves it until
/// something else is copied. The exit code.
pub fn run(mut args: impl Iterator<Item = String>) -> i32 {
    use arboard::SetExtLinux;
    let mut input = Vec::new();
    if let Err(e) = std::io::stdin().read_to_end(&mut input) {
        eprintln!("{ARG}: {e}");
        return 1;
    }
    let mut clipboard = match arboard::Clipboard::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("clipboard unavailable: {e}");
            return 1;
        }
    };
    let set = clipboard.set().wait();
    let result = match args.next().as_deref() {
        Some("image") => {
            let mut size = args.by_ref().take(2).map(|n| n.parse::<usize>().ok());
            match (size.next().flatten(), size.next().flatten()) {
                (Some(width), Some(height)) if width * height * 4 == input.len() => set.image(arboard::ImageData {
                    width,
                    height,
                    bytes: input.into(),
                }),
                _ => return usage(),
            }
        }
        Some("text") => match String::from_utf8(input) {
            Ok(text) => set.text(text),
            Err(_) => return usage(),
        },
        Some("file") => match args.next() {
            Some(path) => set.file_list(&[PathBuf::from(path)]),
            None => return usage(),
        },
        _ => return usage(),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("failed to copy to clipboard: {e}");
            1
        }
    }
}

fn usage() -> i32 {
    eprintln!("usage: snapr {ARG} image <width> <height> | text | file <path>");
    2
}
