//! Usage statistics for the Stats page: how many screenshots and recordings
//! were taken, on which days and at what time of day, and how often the
//! tools were used.
//! Kept in the config folder as `stats.toml`.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Local, NaiveDate, Timelike};
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
    /// Captures on each local date, from the first day counted. Older stats
    /// have none from before snapr started keeping them.
    #[serde(with = "daily_format")]
    pub daily: BTreeMap<NaiveDate, Tally>,
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
            daily: BTreeMap::new(),
        }
    }
}

fn file() -> Option<std::path::PathBuf> {
    Some(Settings::config_dir()?.join("stats.toml"))
}

impl Stats {
    /// The saved stats, or a fresh start counting from now.
    pub fn load() -> Self {
        let Some(path) = file() else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let mut stats: Self = toml::from_str(&text).unwrap_or_else(|e| {
            // Kept, as they'd be saved over with a fresh start.
            let kept = crate::atomic::keep_broken(&path).unwrap_or_default();
            eprintln!("couldn't read stats ({kept}): {e}");
            Self::default()
        });
        stats.captures.normalise();
        stats
    }

    pub fn save(&self) {
        let Some(path) = file() else { return };
        let result = toml::to_string(self)
            .map_err(|e| e.to_string())
            .and_then(|text| crate::atomic::write(&path, text).map_err(|e| e.to_string()));
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
        self.daily.entry(time.date_naive()).or_default().bump(kind);
    }
}

/// `daily` as one line a day, `2026-10-23 = [screenshots, recordings]`, with
/// other files third when there are any. A date that doesn't read is dropped
/// rather than losing the whole file.
mod daily_format {
    use super::*;
    use serde::{Deserializer, Serializer};

    const FORMAT: &str = "%Y-%m-%d";

    pub fn serialize<S: Serializer>(daily: &BTreeMap<NaiveDate, Tally>, s: S) -> Result<S::Ok, S::Error> {
        s.collect_map(daily.iter().map(|(date, t)| {
            let mut counts = vec![t.screenshots, t.recordings];
            if t.other > 0 {
                counts.push(t.other);
            }
            (date.format(FORMAT).to_string(), counts)
        }))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<NaiveDate, Tally>, D::Error> {
        let raw = BTreeMap::<String, Vec<u64>>::deserialize(d)?;
        Ok(raw
            .into_iter()
            .filter_map(|(date, counts)| {
                let date = NaiveDate::parse_from_str(&date, FORMAT).ok()?;
                let n = |i: usize| counts.get(i).copied().unwrap_or(0);
                Some((date, Tally { screenshots: n(0), recordings: n(1), other: n(2) }))
            })
            .collect())
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
        let day = s.daily[&t.date_naive()];
        assert_eq!((day.screenshots, day.recordings), (2, 1));
    }

    #[test]
    fn round_trips_through_toml() {
        let mut s = Stats::default();
        s.qr_scans = 4;
        s.count(Kind::Recording, Local::now());
        let text = toml::to_string(&s).unwrap();
        assert_eq!(toml::from_str::<Stats>(&text).unwrap(), s);
    }

    #[test]
    fn reads_days_and_skips_bad_ones() {
        let s: Stats = toml::from_str(
            "[daily]
\"2026-10-22\" = [4, 1]
\"2026-10-23\" = [2, 0, 7]
\"someday\" = [9, 9]
",
        )
        .unwrap();
        assert_eq!(s.daily.len(), 2);
        let d = NaiveDate::from_ymd_opt(2026, 10, 23).unwrap();
        assert_eq!(s.daily[&d], Tally { screenshots: 2, recordings: 0, other: 7 });
        // Stats from before days were counted.
        assert!(toml::from_str::<Stats>("qr_scans = 3
").unwrap().daily.is_empty());
    }
}
