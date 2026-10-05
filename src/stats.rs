//! Usage statistics for the Stats page: how many screenshots and recordings
//! were taken and at what time of day, and how often the tools were used.
//! Kept in the config folder as `stats.toml`.

use chrono::{DateTime, Datelike, Local, Timelike};
use serde::{Deserialize, Serialize};

use crate::settings::Settings;

pub const WEEKDAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Screenshot,
    Recording,
}

/// Captures of each kind. snapr never counts `other` itself; it holds files
/// that were neither pictures nor videos, in history carried over from elsewhere.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tally {
    pub screenshots: u64,
    pub recordings: u64,
    pub other: u64,
}

impl Tally {
    pub fn total(&self) -> u64 {
        self.screenshots + self.recordings + self.other
    }

    fn bump(&mut self, kind: Kind) {
        match kind {
            Kind::Screenshot => self.screenshots += 1,
            Kind::Recording => self.recordings += 1,
        }
    }
}

/// Captures in all, by hour of the day (0–23) and by day of the week
/// (Monday first).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Captures {
    pub total: Tally,
    pub hours: Vec<Tally>,
    pub days: Vec<Tally>,
}

impl Default for Captures {
    fn default() -> Self {
        Self {
            total: Tally::default(),
            hours: vec![Tally::default(); 24],
            days: vec![Tally::default(); 7],
        }
    }
}

impl Captures {
    /// Pads or trims the lists to 24 hours and 7 days, for a hand-edited file.
    fn normalise(&mut self) {
        self.hours.resize(24, Tally::default());
        self.days.resize(7, Tally::default());
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Stats {
    /// When snapr started counting (Unix seconds).
    pub since: i64,
    pub captures: Captures,
    /// Regions read for QR codes.
    pub qr_scans: u64,
    /// QR codes found in them.
    pub qr_codes: u64,
    pub colors_picked: u64,
    /// Images pinned to the screen.
    pub pins: u64,
}

impl Default for Stats {
    fn default() -> Self {
        Self {
            since: Local::now().timestamp(),
            captures: Captures::default(),
            qr_scans: 0,
            qr_codes: 0,
            colors_picked: 0,
            pins: 0,
        }
    }
}

fn file() -> Option<std::path::PathBuf> {
    Some(Settings::config_dir()?.join("stats.toml"))
}

impl Stats {
    /// The saved stats, or a fresh start counting from now.
    pub fn load() -> Self {
        let Some(text) = file().and_then(|f| std::fs::read_to_string(f).ok()) else {
            return Self::default();
        };
        let mut stats: Self = toml::from_str(&text).unwrap_or_else(|e| {
            eprintln!("couldn't read stats: {e}");
            Self::default()
        });
        stats.captures.normalise();
        stats
    }

    pub fn save(&self) {
        let Some(path) = file() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let result = toml::to_string(self)
            .map_err(|e| e.to_string())
            .and_then(|text| std::fs::write(&path, text).map_err(|e| e.to_string()));
        if let Err(e) = result {
            eprintln!("couldn't save stats: {e}");
        }
    }

    /// Counts a screenshot or recording taken at `time`.
    pub fn count(&mut self, kind: Kind, time: DateTime<Local>) {
        let c = &mut self.captures;
        c.total.bump(kind);
        c.hours[time.hour() as usize].bump(kind);
        c.days[time.weekday().num_days_from_monday() as usize].bump(kind);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_by_hour_and_day() {
        use chrono::TimeZone;
        let mut s = Stats::default();
        // A Monday afternoon.
        let t = Local.with_ymd_and_hms(2026, 10, 5, 14, 30, 0).unwrap();
        s.count(Kind::Screenshot, t);
        s.count(Kind::Screenshot, t);
        s.count(Kind::Recording, t);
        assert_eq!(s.captures.total.screenshots, 2);
        assert_eq!(s.captures.total.recordings, 1);
        assert_eq!(s.captures.hours[14].total(), 3);
        assert_eq!(s.captures.days[0].total(), 3);
    }

    #[test]
    fn round_trips_through_toml() {
        let mut s = Stats::default();
        s.qr_scans = 4;
        s.count(Kind::Recording, Local::now());
        let text = toml::to_string(&s).unwrap();
        assert_eq!(toml::from_str::<Stats>(&text).unwrap(), s);
    }
}
