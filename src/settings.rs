//! User settings, stored as TOML in the OS config folder.

use std::path::PathBuf;

use livesplit_hotkey::Hotkey;
use serde::{Deserialize, Serialize};

use crate::naming::{Naming, Template, migrate_legacy};
use crate::pomf::PomfTarget;
use crate::upload::{Body, Progress, S3Target, Target, encode_path};

pub const DEFAULT_HOTKEY: &str = "Alt + Shift + KeyS";
pub const DEFAULT_RECORD_HOTKEY: &str = "Alt + Shift + KeyV";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub hotkey: String,
    /// Starts recording a region, and stops it. Empty: no hotkey.
    pub record_hotkey: String,
    /// Base folder screenshots are saved in.
    pub folder: String,
    /// Sub-folder template, e.g. `%pn/%y-%mo`.
    pub subfolder: String,
    /// File name template, `.png` is added.
    pub file_name: String,
    /// Save each screenshot to the folder above.
    pub save_to_folder: bool,
    pub copy_to_clipboard: bool,
    /// After uploading, copy the link to the clipboard (replacing the image).
    pub copy_link: bool,
    /// S3-compatible upload destinations.
    pub uploads: Vec<Upload>,
    /// Show the annotation toolbar while capturing. Either way, releasing a
    /// region drag takes the screenshot.
    pub annotate: bool,
    /// Shutter sound on capture, chime/error sounds for results.
    pub play_sounds: bool,
    /// How loud those sounds are, in percent.
    pub sound_volume: u8,
    /// Pop up a preview in the corner of the screen after each capture.
    pub show_toast: bool,
    /// Overlay frame-rate limit; 0 matches each monitor's refresh rate.
    pub overlay_fps: u32,
    /// What's shown beside the crosshair while selecting a region.
    pub crosshair_info: CrosshairInfo,
    /// The picture the image redaction tool covers areas with; empty draws
    /// black boxes.
    pub redact_image: String,
    /// Squash or stretch the redaction image to each area's shape, rather
    /// than keeping its proportions and cropping it.
    pub redact_stretch: bool,
    /// Frame rate of screen recordings.
    pub record_fps: u32,
    /// File format of screen recordings.
    pub record_format: RecordFormat,
    /// Record displays in HDR mode in HDR (HDR10; Windows, MP4 only).
    pub record_hdr: bool,
    /// Encode MP4 (H.264) and WebM recordings with FFmpeg rather than the
    /// system's encoder (Windows and macOS; Linux always uses FFmpeg).
    pub record_with_ffmpeg: bool,
    /// The FFmpeg program videos are encoded and decoded with on Linux, and
    /// recordings encoded with when `record_with_ffmpeg` is on; empty finds
    /// it on PATH.
    pub ffmpeg_path: String,
    /// How FFmpeg encodes recordings.
    pub ffmpeg: FfmpegOptions,
    /// Record what's playing along with the screen.
    pub record_system_audio: bool,
    /// Draw the mouse pointer into recordings.
    pub record_cursor: bool,
    /// Record a microphone along with the screen.
    pub record_microphone: bool,
    /// The microphone's name; empty for the system default.
    pub microphone: String,
    /// Global hotkeys that run a tool straight away.
    pub tool_hotkeys: Vec<ToolHotkey>,
    /// How upload speeds are shown.
    pub speed_unit: SpeedUnit,
    /// Look for a new release now and then, and install it.
    pub check_for_updates: bool,
}

/// Details shown beside the crosshair while selecting a region. The colour
/// picker always shows the magnifier and colour, and the position if it's on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CrosshairInfo {
    /// The pixel's position on the screen.
    pub position: bool,
    /// The pixel's colour.
    pub color: bool,
    /// The pixels around the cursor, enlarged.
    pub magnifier: bool,
    /// What the mouse wheel does to the magnifier (the colour picker's too).
    pub scroll: MagnifierScroll,
}

impl CrosshairInfo {
    pub fn any(self) -> bool {
        self.position || self.color || self.magnifier
    }
}

/// What scrolling over the magnifier does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MagnifierScroll {
    /// Fewer, bigger pixels in the same size magnifier, or more, smaller ones.
    #[default]
    Zoom,
    /// The same size pixels in a bigger or smaller magnifier, down to hiding
    /// it.
    Resize,
}

/// Recordings as MP4 (H.264 + AAC; plays everywhere), MP4 with AV1 video
/// (sharper for the size; Windows with a GPU that encodes AV1) or WebM
/// (VP9 + Opus; Windows and Linux only).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordFormat {
    #[default]
    Mp4,
    Av1,
    Webm,
}

impl RecordFormat {
    /// The file extension, falling back to MP4 where WebM can't be made.
    pub fn extension(self) -> &'static str {
        match self {
            RecordFormat::Webm if crate::encode::webm_supported() => "webm",
            _ => "mp4",
        }
    }
}

/// How FFmpeg encodes recordings: x264 for MP4, libvpx for WebM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FfmpegOptions {
    /// x264's preset, one of `X264_PRESETS`: slower ones make smaller files
    /// at the same quality, for more CPU.
    pub x264_preset: String,
    /// x264's constant rate factor, 0 (lossless) to 51: lower is better
    /// quality and bigger files.
    pub x264_crf: u8,
    /// libvpx's speed (`-cpu-used`), 0 (slowest, smallest files) to 8.
    pub vp9_speed: u8,
    /// libvpx's constant rate factor, 0 to 63: lower is better quality and
    /// bigger files, up to the usual bit rate.
    pub vp9_crf: u8,
    /// More arguments for the output, after snapr's own (so they win), split
    /// like a shell command line.
    pub extra_args: String,
}

impl Default for FfmpegOptions {
    fn default() -> Self {
        Self {
            x264_preset: "veryfast".into(),
            x264_crf: 23,
            vp9_speed: 8,
            vp9_crf: 32,
            extra_args: String::new(),
        }
    }
}

impl FfmpegOptions {
    pub const X264_PRESETS: &[&str] = &[
        "ultrafast",
        "superfast",
        "veryfast",
        "faster",
        "fast",
        "medium",
        "slow",
        "slower",
        "veryslow",
    ];

    /// `extra_args` split into separate arguments.
    pub fn extra_args(&self) -> Result<Vec<String>, String> {
        shlex::split(&self.extra_args).ok_or_else(|| "a quote isn't closed".into())
    }
}

/// Upload speeds in bytes (MB/s, like file sizes) or bits (Mbps, like
/// internet plans).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeedUnit {
    #[default]
    Bytes,
    Bits,
}

/// A global hotkey that runs one of the tools.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolHotkey {
    pub action: ToolAction,
    /// e.g. `Ctrl + Shift + KeyC`.
    pub hotkey: String,
}

/// A tool that can be given a hotkey.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolAction {
    PickColor,
    ScanQr,
    PinRegion,
}

impl ToolAction {
    pub const ALL: [ToolAction; 3] = [
        ToolAction::PickColor,
        ToolAction::ScanQr,
        ToolAction::PinRegion,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ToolAction::PickColor => "Pick a colour",
            ToolAction::ScanQr => "Read a QR code",
            ToolAction::PinRegion => "Pin a region to the screen",
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: DEFAULT_HOTKEY.into(),
            record_hotkey: DEFAULT_RECORD_HOTKEY.into(),
            folder: default_folder().display().to_string(),
            subfolder: "%y-%mo".into(),
            file_name: "%rna{10}".into(),
            save_to_folder: true,
            copy_to_clipboard: true,
            copy_link: true,
            uploads: Vec::new(),
            annotate: true,
            play_sounds: true,
            sound_volume: 100,
            show_toast: true,
            overlay_fps: 0,
            crosshair_info: CrosshairInfo::default(),
            redact_image: String::new(),
            redact_stretch: false,
            record_fps: 30,
            record_format: RecordFormat::default(),
            record_hdr: true,
            record_with_ffmpeg: false,
            ffmpeg_path: String::new(),
            ffmpeg: FfmpegOptions::default(),
            record_cursor: true,
            record_system_audio: false,
            record_microphone: false,
            microphone: String::new(),
            tool_hotkeys: Vec::new(),
            speed_unit: SpeedUnit::default(),
            check_for_updates: true,
        }
    }
}

pub enum Loaded {
    FirstRun,
    Existing,
    /// The file exists but couldn't be read; defaults are used instead.
    Broken(String),
}

impl Settings {
    /// `SNAPR_CONFIG_DIR` overrides the location, e.g. for a portable copy or
    /// to run a second, independent instance.
    pub fn config_dir() -> Option<PathBuf> {
        if let Some(dir) = std::env::var_os("SNAPR_CONFIG_DIR") {
            return Some(PathBuf::from(dir));
        }
        Some(dirs::config_dir()?.join("snapr"))
    }

    fn path() -> Option<PathBuf> {
        Some(Self::config_dir()?.join("config.toml"))
    }

    pub fn load() -> (Self, Loaded) {
        let Some(path) = Self::path() else {
            return (Self::default(), Loaded::FirstRun);
        };
        match std::fs::read_to_string(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                (Self::default(), Loaded::FirstRun)
            }
            Err(e) => (
                Self::default(),
                Loaded::Broken(format!("couldn't read {}: {e}", path.display())),
            ),
            Ok(text) => match toml::from_str::<Self>(&text) {
                Ok(mut s) => {
                    s.migrate_templates();
                    (s, Loaded::Existing)
                }
                Err(e) => (
                    Self::default(),
                    Loaded::Broken(format!("couldn't parse {}: {e}", path.display())),
                ),
            },
        }
    }

    /// Rewrites templates from the old `{random}` syntax to ShareX's `%rna{10}`.
    fn migrate_templates(&mut self) {
        for t in [&mut self.subfolder, &mut self.file_name]
            .into_iter()
            .chain(self.uploads.iter_mut().map(|u| &mut u.key_template))
        {
            *t = migrate_legacy(t);
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::path().ok_or("no config folder on this system")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("couldn't create {}: {e}", dir.display()))?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| format!("couldn't write {}: {e}", path.display()))
    }

    /// The sound to record with the screen.
    pub fn audio_sources(&self) -> Vec<crate::audio::Source> {
        let mut sources = Vec::new();
        if self.record_system_audio {
            sources.push(crate::audio::Source::System);
        }
        if self.record_microphone {
            sources.push(crate::audio::Source::Microphone(
                self.microphone.trim().to_string(),
            ));
        }
        sources
    }

    pub fn naming(&self) -> Result<Naming, String> {
        Ok(Naming {
            dir: PathBuf::from(&self.folder),
            subdir: self
                .subfolder
                .parse::<Template>()
                .map_err(|e| format!("Sub-folder: {e}"))?,
            name: self
                .file_name
                .parse::<Template>()
                .map_err(|e| format!("File name: {e}"))?,
        })
    }
}

/// The kind of service an upload destination is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UploadKind {
    /// An S3-compatible bucket.
    #[default]
    S3,
    /// A Pomf-compatible file host (version 1 of its API).
    Pomf,
}

/// Where screenshots are uploaded to: an S3-compatible bucket, whose secret
/// key is kept in the OS credential store under `id`, or a Pomf host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Upload {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub kind: UploadKind,
    /// The storage endpoint, or for Pomf the upload address (e.g.
    /// `https://pomf.lain.la/upload.php`).
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    /// Object key template, using the file-name placeholders; `.png` is added.
    /// Pomf hosts are sent its last part as the file name, which most
    /// replace with their own.
    pub key_template: String,
    /// Base for links, e.g. `https://pub-xxxx.r2.dev` or a custom domain.
    /// Empty uses the storage endpoint's own URL, or the link the Pomf host
    /// gives.
    pub public_url: String,
    pub path_style: bool,
    /// Include a hash of the file in the request signature.
    pub signed_payload: bool,
    /// Parts of a large upload sent at the same time.
    pub parallel_parts: u32,
}

impl Default for Upload {
    fn default() -> Self {
        Self {
            id: (0..12)
                .map(|_| fastrand::alphanumeric())
                .collect::<String>()
                .to_lowercase(),
            name: "S3".into(),
            enabled: true,
            kind: UploadKind::S3,
            endpoint: String::new(),
            region: "us-east-1".into(),
            bucket: String::new(),
            access_key_id: String::new(),
            key_template: "%y-%mo/%rna{10}".into(),
            public_url: String::new(),
            path_style: false,
            signed_payload: false,
            parallel_parts: crate::upload::DEFAULT_CONCURRENCY,
        }
    }
}

impl Upload {
    /// A new Pomf destination.
    pub fn pomf() -> Self {
        Self {
            name: "Pomf".into(),
            kind: UploadKind::Pomf,
            key_template: "%rna{10}".into(),
            ..Self::default()
        }
    }

    /// Whether it needs a secret key from the credential store.
    pub fn needs_secret(&self) -> bool {
        self.kind == UploadKind::S3
    }

    /// Whether what's uploaded there can be deleted again.
    pub fn deletable(&self) -> bool {
        self.kind == UploadKind::S3
    }

    /// Checks the fields; returns the parsed key template.
    pub fn validate(&self) -> Result<Template, String> {
        let endpoint = self.endpoint.trim();
        if !(endpoint.starts_with("https://") || endpoint.starts_with("http://")) {
            return Err(match self.kind {
                UploadKind::S3 => "Endpoint should start with https://",
                UploadKind::Pomf => "Upload URL should start with https://",
            }
            .into());
        }
        if self.kind == UploadKind::S3 {
            if self.bucket.trim().is_empty() {
                return Err("Bucket is required".into());
            }
            if self.access_key_id.trim().is_empty() {
                return Err("Access key ID is required".into());
            }
        }
        let public = self.public_url.trim();
        if !(public.is_empty() || public.starts_with("https://") || public.starts_with("http://")) {
            return Err("Public URL should start with https://".into());
        }
        self.key_template.parse::<Template>().map_err(|e| match self.kind {
            UploadKind::S3 => format!("Object key: {e}"),
            UploadKind::Pomf => format!("File name: {e}"),
        })
    }

    /// Where its files go; the secret key is only used for S3.
    pub fn target(&self, secret_access_key: String) -> Target {
        match self.kind {
            UploadKind::S3 => Target::S3(self.s3_target(secret_access_key)),
            UploadKind::Pomf => Target::Pomf(PomfTarget {
                url: self.endpoint.trim().to_string(),
                public_url: self.public_url.trim().to_string(),
            }),
        }
    }

    /// Uploads `body` to `target` (from [`Self::target`]) as `key`, following
    /// along in `progress`. Returns its link.
    pub fn put(
        &self,
        target: &Target,
        key: &str,
        body: &Body,
        content_type: &str,
        progress: &Progress,
    ) -> Result<String, String> {
        match target {
            Target::S3(t) => {
                t.put_object(key, body, content_type, progress)?;
                self.link(key)
            }
            Target::Pomf(t) => t.upload(key, body, content_type, progress),
        }
    }

    fn s3_target(&self, secret_access_key: String) -> S3Target {
        S3Target {
            endpoint: self.endpoint.trim().to_string(),
            region: if self.region.trim().is_empty() {
                "auto".into()
            } else {
                self.region.trim().to_string()
            },
            bucket: self.bucket.trim().to_string(),
            access_key_id: self.access_key_id.trim().to_string(),
            // A pasted secret often brings a stray space or newline along.
            secret_access_key: secret_access_key.trim().to_string(),
            path_style: self.path_style,
            sign_payload: self.signed_payload,
            concurrency: self.parallel_parts,
        }
    }

    /// The link to share for an uploaded object.
    pub fn link(&self, key: &str) -> Result<String, String> {
        let public = self.public_url.trim().trim_end_matches('/');
        if public.is_empty() {
            self.s3_target(String::new()).object_url(key)
        } else {
            Ok(format!("{public}/{}", encode_path(key)))
        }
    }
}

pub fn parse_hotkey(s: &str) -> Result<Hotkey, String> {
    s.parse()
        .map_err(|_| format!("not a valid hotkey (expected e.g. \"{DEFAULT_HOTKEY}\")"))
}

pub fn default_folder() -> PathBuf {
    dirs::picture_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("snapr")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_hotkeys_round_trip() {
        let settings = Settings {
            tool_hotkeys: vec![ToolHotkey {
                action: ToolAction::PickColor,
                hotkey: "Ctrl + Shift + KeyC".into(),
            }],
            ..Settings::default()
        };
        let text = toml::to_string_pretty(&settings).unwrap();
        assert!(text.contains("[[tool_hotkeys]]"), "{text}");
        assert!(text.contains("action = \"pick_color\""), "{text}");
        assert_eq!(toml::from_str::<Settings>(&text).unwrap(), settings);
        // Older config files have none.
        let old: Settings = toml::from_str("hotkey = \"Alt + Shift + KeyS\"").unwrap();
        assert!(old.tool_hotkeys.is_empty());
    }
}
