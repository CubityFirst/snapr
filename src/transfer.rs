//! Settings exported to a file and imported from one, to move them to
//! another computer: the Settings window's, the capture tools' last ones and,
//! if asked for, the upload secret keys.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::remembered::Remembered;
use crate::secrets;
use crate::settings::{Settings, Upload, default_folder};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Exported {
    /// The snapr that wrote it.
    pub snapr: String,
    pub settings: Settings,
    #[serde(default)]
    pub tools: Option<Remembered>,
    /// Upload secret keys by destination id, when they were included.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secrets: BTreeMap<String, String>,
}

/// What came in from a file, ready to be saved.
pub struct Imported {
    pub settings: Settings,
    pub tools: Option<Remembered>,
    pub secrets: BTreeMap<String, String>,
    /// What was changed to suit this computer, to tell the user.
    pub notes: Vec<String>,
}

/// Writes `settings`, the tools' saved settings and (with `with_secrets`)
/// the upload secret keys to `path`. Returns how many keys went in.
pub fn export(path: &Path, settings: &Settings, with_secrets: bool) -> Result<usize, String> {
    let secrets: BTreeMap<String, String> = if with_secrets {
        settings
            .uploads
            .iter()
            .filter(|u| u.needs_secret())
            .filter_map(|u| Some((u.id.clone(), secrets::get(u)?)))
            .collect()
    } else {
        BTreeMap::new()
    };
    let exported = Exported {
        snapr: crate::update::VERSION.to_string(),
        settings: settings.clone(),
        tools: Some(Remembered::load()),
        secrets,
    };
    let mut text = String::from("# snapr settings: import them on the General tab of Settings.\n");
    if !exported.secrets.is_empty() {
        text.push_str("# Holds upload secret keys: keep it private.\n");
    }
    text.push('\n');
    text.push_str(&toml::to_string(&exported).map_err(|e| e.to_string())?);
    crate::atomic::write(path, text).map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
    Ok(exported.secrets.len())
}

/// Reads settings exported from snapr, fitted to this computer.
pub fn import(path: &Path) -> Result<Imported, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("couldn't read {}: {e}", path.display()))?;
    let exported: Exported = toml::from_str(&text)
        .map_err(|e| format!("{} isn't a snapr settings file: {e}", path.display()))?;
    Ok(fit(exported, |u| secrets::get(u).is_some()))
}

/// Resets paths that don't exist here, and turns off destinations with no
/// secret key in the file or on this computer (`has_secret`).
fn fit(exported: Exported, has_secret: impl Fn(&Upload) -> bool) -> Imported {
    let Exported {
        mut settings,
        tools,
        secrets,
        ..
    } = exported;
    settings.fix_duplicate_ids();
    let mut notes = Vec::new();
    let folder = Path::new(settings.folder.trim());
    if !folder.exists() && !folder.parent().is_some_and(Path::exists) {
        settings.folder = default_folder().display().to_string();
        notes.push(format!("the save folder isn't on this computer, so it's {}", settings.folder));
    }
    let redact = settings.redact_image.trim();
    if !redact.is_empty() && !Path::new(redact).is_file() {
        settings.redact_image.clear();
        notes.push("the redaction image isn't on this computer, so it's cleared".into());
    }
    let ffmpeg = settings.ffmpeg_path.trim();
    if !ffmpeg.is_empty() && !Path::new(ffmpeg).is_file() {
        settings.ffmpeg_path.clear();
        notes.push("FFmpeg isn't where it was, so it's looked for on the PATH".into());
    }
    for u in &mut settings.uploads {
        if u.enabled && u.needs_secret() && !secrets.contains_key(&u.id) && !has_secret(u) {
            u.enabled = false;
            notes.push(format!("{} is off until its secret key is entered on Destinations", u.name));
        }
    }
    Imported {
        settings,
        tools,
        secrets,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Upload;

    fn exported() -> Exported {
        let with_key = Upload {
            name: "With key".into(),
            ..Default::default()
        };
        let without_key = Upload {
            name: "Without key".into(),
            ..Default::default()
        };
        let settings = Settings {
            hotkey: "Ctrl + Shift + KeyA".into(),
            folder: "Q:\\nowhere\\at\\all\\snapr".into(),
            redact_image: "Q:\\nowhere\\face.png".into(),
            uploads: vec![with_key.clone(), without_key, Upload::pomf()],
            ..Default::default()
        };
        Exported {
            snapr: "0.0.0".into(),
            settings,
            tools: Some(Remembered::default()),
            secrets: BTreeMap::from([(with_key.id, "secret".into())]),
        }
    }

    #[test]
    fn round_trips_through_toml() {
        let e = exported();
        let text = toml::to_string(&e).unwrap();
        assert_eq!(toml::from_str::<Exported>(&text).unwrap(), e);
    }

    #[test]
    fn exports_and_imports_a_file() {
        let path = std::env::temp_dir().join(format!("snapr-export-{}.toml", fastrand::u64(..)));
        let settings = Settings {
            hotkey: "Ctrl + Shift + KeyA".into(),
            uploads: vec![Upload::pomf()],
            ..Default::default()
        };
        assert_eq!(export(&path, &settings, false).unwrap(), 0);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# snapr settings"));
        assert!(!text.contains("[secrets]"));
        let imported = import(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(imported.settings, settings);
        assert!(imported.tools.is_some());
        assert!(imported.notes.is_empty());

        std::fs::write(&path, "hotkey = 3").unwrap();
        assert!(import(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fits_paths_and_keys_to_this_computer() {
        let e = exported();
        let imported = fit(e.clone(), |_| false);
        let s = &imported.settings;
        assert_eq!(s.hotkey, e.settings.hotkey);
        assert_eq!(s.folder, default_folder().display().to_string());
        assert!(s.redact_image.is_empty());
        // The key came along; the other S3 one has none, Pomf needs none.
        let enabled: Vec<bool> = s.uploads.iter().map(|u| u.enabled).collect();
        assert_eq!(enabled, [true, false, true]);
        assert_eq!(imported.notes.len(), 3);
        assert_eq!(imported.secrets, e.secrets);

        // A key already stored here keeps it on.
        let imported = fit(e, |_| true);
        assert!(imported.settings.uploads.iter().all(|u| u.enabled));
    }
}
