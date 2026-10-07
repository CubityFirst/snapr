//! Updates from the GitHub releases the release workflow publishes
//! (`.github/workflows/release.yml`): finds the latest release, downloads
//! this platform's build, checks it against the release's `SHA256SUMS` and
//! puts it in place of the running program, which carries on as it is until
//! it's restarted.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How long after starting the first automatic check waits, so it doesn't
/// slow down logging in.
const FIRST_CHECK: Duration = Duration::from_secs(15);
const CHECK_EVERY: Duration = Duration::from_secs(12 * 60 * 60);

/// Where the updater is up to, shown in Settings.
#[derive(Debug, Clone, Default)]
pub enum Status {
    #[default]
    Idle,
    Checking,
    /// When it last found nothing newer.
    UpToDate(chrono::DateTime<chrono::Local>),
    Downloading(String),
    /// A newer version that development builds don't install themselves
    /// over: its version and release page.
    Available(String, String),
    /// Installed; it runs from the next start. Its version and release page.
    Installed(String, String),
    Failed(String),
}

impl Status {
    pub fn busy(&self) -> bool {
        matches!(self, Status::Checking | Status::Downloading(_))
    }
}

/// The program's own file, as it was when snapr started: once replaced,
/// `current_exe` can name the old copy instead.
static EXE: OnceLock<Option<PathBuf>> = OnceLock::new();
/// A check is running.
static BUSY: AtomicBool = AtomicBool::new(false);
/// The version installed this run, so later checks don't fetch it again.
static INSTALLED: std::sync::Mutex<Option<(String, String)>> = std::sync::Mutex::new(None);

/// Remembers where the program is and removes the copy an update left
/// behind. Call first thing.
pub fn init() {
    let exe = EXE.get_or_init(|| std::env::current_exe().ok());
    if let Some(exe) = exe {
        remove_old_copies(exe);
    }
}

pub fn exe() -> Result<&'static Path, String> {
    EXE.get_or_init(|| std::env::current_exe().ok())
        .as_deref()
        .ok_or_else(|| "couldn't find snapr's own file".into())
}

/// Calls `due` shortly after starting, then twice a day.
pub fn schedule(due: impl Fn() + Send + 'static) {
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_CHECK);
        loop {
            due();
            std::thread::sleep(CHECK_EVERY);
        }
    });
}

/// Checks for a newer release and installs it, off the calling thread,
/// reporting each step. Does nothing while a check is already running.
pub fn check(report: impl Fn(Status) + Send + 'static) {
    if BUSY.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(move || {
        report(Status::Checking);
        let status = check_and_install(&report).unwrap_or_else(Status::Failed);
        BUSY.store(false, Ordering::Release);
        report(status);
    });
}

/// Starts the installed program again, e.g. once this one has exited after
/// an update.
pub fn restart() -> Result<(), String> {
    std::process::Command::new(exe()?)
        .spawn()
        .map(drop)
        .map_err(|e| format!("couldn't start snapr again: {e}"))
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
}

fn check_and_install(report: &dyn Fn(Status)) -> Result<Status, String> {
    let release = latest_release()?;
    let version = release.tag_name.trim_start_matches('v').to_string();
    if !newer(&version, VERSION) {
        return Ok(Status::UpToDate(chrono::Local::now()));
    }
    if let Some((v, page)) = INSTALLED.lock().unwrap().clone()
        && !newer(&version, &v)
    {
        return Ok(Status::Installed(v, page));
    }
    if cfg!(debug_assertions) {
        return Ok(Status::Available(version, release.html_url));
    }
    let name = asset_name();
    let find = |name: &str| release.assets.iter().find(|a| a.name == name);
    let asset = find(&name).ok_or_else(|| format!("{} has no build for {name}", release.tag_name))?;
    let sums = find("SHA256SUMS")
        .ok_or_else(|| format!("{} has no SHA256SUMS to check the download with", release.tag_name))?;

    report(Status::Downloading(version.clone()));
    let mut text = String::new();
    get(&sums.browser_download_url)?
        .read_to_string(&mut text)
        .map_err(|e| format!("couldn't download SHA256SUMS: {e}"))?;
    let expected = checksum_for(&text, &name)
        .ok_or_else(|| format!("SHA256SUMS doesn't list {name}"))?;

    let exe = exe()?;
    let new = sibling(exe, "new");
    let result = download(asset, &new, &expected).and_then(|()| replace(exe, &new));
    if result.is_err() {
        let _ = std::fs::remove_file(&new);
    }
    result?;
    *INSTALLED.lock().unwrap() = Some((version.clone(), release.html_url.clone()));
    Ok(Status::Installed(version, release.html_url))
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(Duration::from_secs(10 * 60)))
        .user_agent(format!("snapr/{VERSION}"))
        .build()
        .into()
}

/// The response body for a GET, or why there isn't one.
fn get(url: &str) -> Result<impl Read, String> {
    let response = agent()
        .get(url)
        .call()
        .map_err(|e| format!("couldn't reach GitHub: {e}"))?;
    match response.status().as_u16() {
        200 => Ok(response.into_body().into_reader()),
        403 | 429 => Err("GitHub is limiting requests; try again later".into()),
        404 => Err("no releases have been published yet".into()),
        code => Err(format!("GitHub answered {code} for {url}")),
    }
}

fn latest_release() -> Result<Release, String> {
    let repo = env!("CARGO_PKG_REPOSITORY")
        .trim_start_matches("https://github.com/")
        .trim_end_matches('/');
    let mut text = String::new();
    get(&format!("https://api.github.com/repos/{repo}/releases/latest"))?
        .read_to_string(&mut text)
        .map_err(|e| format!("couldn't read the latest release: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("couldn't read the latest release: {e}"))
}

/// This platform's build among a release's files, e.g.
/// `snapr-windows-x86_64.exe`, `snapr-macos-aarch64`, `snapr-linux-x86_64`.
fn asset_name() -> String {
    use std::env::consts::{ARCH, EXE_SUFFIX, OS};
    format!("snapr-{OS}-{ARCH}{EXE_SUFFIX}")
}

/// Whether version `a` comes after `b`: `1.10.0` > `1.9.2`, and a release
/// after its own pre-releases (`1.0.0` > `1.0.0-rc.1`).
fn newer(a: &str, b: &str) -> bool {
    fn parse(v: &str) -> (Vec<u64>, bool) {
        let (core, pre) = match v.split_once(['-', '+']) {
            Some((core, rest)) => (core, !rest.is_empty() && v.as_bytes()[core.len()] == b'-'),
            None => (v, false),
        };
        let mut parts: Vec<u64> = core.split('.').map(|p| p.parse().unwrap_or(0)).collect();
        parts.resize(3, 0);
        (parts, pre)
    }
    let (a, a_pre) = parse(a.trim());
    let (b, b_pre) = parse(b.trim());
    a > b || (a == b && b_pre && !a_pre)
}

/// The checksum `SHA256SUMS` (`sha256sum`'s output) gives a file.
fn checksum_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.trim().split_once(char::is_whitespace)?;
        // `*` marks a file hashed in binary mode.
        let file = file.trim_start().trim_start_matches('*');
        (file == name).then(|| hash.to_ascii_lowercase())
    })
}

/// Downloads `asset` to `to`, checking its size and checksum.
fn download(asset: &Asset, to: &Path, expected: &str) -> Result<(), String> {
    let fail = |e: std::io::Error| format!("couldn't download {}: {e}", asset.name);
    let mut body = get(&asset.browser_download_url)?;
    let mut file = std::fs::File::create(to)
        .map_err(|e| format!("couldn't write {}: {e}", to.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0; 64 * 1024];
    let mut size = 0;
    loop {
        let n = body.read(&mut buf).map_err(fail)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(fail)?;
        size += n as u64;
    }
    file.sync_all().map_err(fail)?;
    if size != asset.size {
        return Err(format!(
            "the download of {} stopped short ({size} of {} bytes)",
            asset.name, asset.size
        ));
    }
    let hash = hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        });
    if hash != expected {
        return Err(format!("{} doesn't match its checksum", asset.name));
    }
    Ok(())
}

/// `snapr.exe` → `snapr.exe.<suffix>`, next to it.
fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(suffix);
    exe.with_file_name(name)
}

/// Puts the downloaded program `new` in place of `exe`, which may be
/// running.
#[cfg(unix)]
fn replace(exe: &Path, new: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(new, std::fs::Permissions::from_mode(0o755))
        .and_then(|()| std::fs::rename(new, exe))
        .map_err(|e| format!("couldn't replace {}: {e}", exe.display()))
}

/// Puts the downloaded program `new` in place of `exe`, which may be
/// running. Windows won't delete a running program but will rename it, so
/// it's moved aside, and removed by `init` next time.
#[cfg(windows)]
fn replace(exe: &Path, new: &Path) -> Result<(), String> {
    let mut old = sibling(exe, "old");
    // An earlier update this run left the program running from there.
    if old.exists() && std::fs::remove_file(&old).is_err() {
        old = sibling(exe, &format!("old{}", fastrand::u32(..)));
    }
    let fail = |e: std::io::Error| format!("couldn't replace {}: {e}", exe.display());
    std::fs::rename(exe, &old).map_err(fail)?;
    if let Err(e) = std::fs::rename(new, exe) {
        let _ = std::fs::rename(&old, exe);
        return Err(fail(e));
    }
    Ok(())
}

/// Removes the copies `replace` moved aside on Windows (`snapr.exe.old…`),
/// and any download a crash left behind.
fn remove_old_copies(exe: &Path) {
    let (Some(dir), Some(name)) = (exe.parent(), exe.file_name()) else {
        return;
    };
    let name = name.to_string_lossy();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let file = entry.file_name();
        let file = file.to_string_lossy();
        if let Some(rest) = file.strip_prefix(&*name)
            && (rest.starts_with(".old") || rest == ".new")
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(newer("0.2.0", "0.1.0"));
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("1.0", "0.9.9"));
        assert!(newer("1.0.0", "1.0.0-rc.1"));
        assert!(!newer("1.0.0-rc.1", "1.0.0"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
        assert!(!newer("0.1.0+build.5", "0.1.0"));
    }

    #[test]
    fn reads_checksums() {
        let sums = "ABC123  snapr-windows-x86_64.exe\n\
                    def456 *snapr-linux-x86_64\n";
        assert_eq!(
            checksum_for(sums, "snapr-windows-x86_64.exe").as_deref(),
            Some("abc123")
        );
        assert_eq!(checksum_for(sums, "snapr-linux-x86_64").as_deref(), Some("def456"));
        assert_eq!(checksum_for(sums, "snapr-macos-aarch64"), None);
    }

    #[test]
    fn names_siblings() {
        assert_eq!(
            sibling(Path::new("dir/snapr.exe"), "old"),
            Path::new("dir/snapr.exe.old")
        );
        assert_eq!(sibling(Path::new("dir/snapr"), "new"), Path::new("dir/snapr.new"));
    }
}
