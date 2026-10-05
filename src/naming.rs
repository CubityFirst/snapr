//! Builds screenshot paths from ShareX-style templates such as `%pn/%y-%mo`
//! and `%rna{10}`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::LazyLock;

use chrono::{DateTime, Local};

/// Base56 for `%rna`: alphanumerics without the look-alikes 0/O/o and 1/I/l.
const UNAMBIGUOUS: &[u8] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz";
const ALPHANUMERIC: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const DIGITS: &[u8] = b"0123456789";
const HEX: &[u8] = b"0123456789ABCDEF";
/// Longest `{n}` repeat count.
const MAX_REPEAT: usize = 64;
/// Longest window title used in a name.
const MAX_TITLE_CHARS: usize = 80;

/// Placeholders by category, as offered by the settings window's Insert menu:
/// (placeholder, description).
pub const CATEGORIES: &[(&str, &[(&str, &str)])] = &[
    ("Window", &[("%pn", "Process name"), ("%t", "Window title")]),
    (
        "Date and time",
        &[
            ("%y", "Year (2026)"),
            ("%yy", "Year (26)"),
            ("%mo", "Month (10)"),
            ("%mon", "Month name (October)"),
            ("%mon2", "Month name (Oct)"),
            ("%d", "Day"),
            ("%h", "Hour (12-hour if %pm is used)"),
            ("%mi", "Minute"),
            ("%s", "Second"),
            ("%ms", "Millisecond"),
            ("%pm", "AM/PM"),
            ("%w", "Weekday (Sunday)"),
            ("%w2", "Weekday (Sun)"),
            ("%wy", "Week of year"),
            ("%unix", "Unix timestamp"),
        ],
    ),
    (
        "Incremental",
        &[
            ("%i", "Number that goes up with each screenshot"),
            ("%i{4}", "Counter padded to 4 digits"),
        ],
    ),
    (
        "Random",
        &[
            ("%rna{10}", "10 characters, no look-alikes like 0/O"),
            ("%ra{10}", "10 alphanumeric characters"),
            ("%rn{6}", "6 digits"),
            ("%rx{8}", "8 hex digits"),
            ("%guid", "GUID"),
        ],
    ),
    (
        "Image",
        &[
            ("%width", "Width in pixels"),
            ("%height", "Height in pixels"),
        ],
    ),
    (
        "Computer",
        &[("%un", "User name"), ("%cn", "Computer name")],
    ),
];

static USER: LazyLock<String> =
    LazyLock::new(|| whoami::account().unwrap_or_else(|_| "user".into()));
static COMPUTER: LazyLock<String> =
    LazyLock::new(|| whoami::hostname().unwrap_or_else(|_| "computer".into()));

/// What a screenshot's name can be derived from.
#[derive(Debug, Clone)]
pub struct Context {
    pub time: DateTime<Local>,
    /// Process and title of the window focused when capturing.
    pub process: Option<String>,
    pub title: Option<String>,
    pub width: u32,
    pub height: u32,
    /// Value for `%i`, filled in just before naming.
    pub counter: u64,
}

impl Context {
    pub fn new(time: DateTime<Local>) -> Self {
        Self {
            time,
            process: None,
            title: None,
            width: 0,
            height: 0,
            counter: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Part {
    Text(String),
    /// Count, then the characters to pick from.
    Random(usize, &'static [u8]),
    Guid,
    /// strftime format.
    Time(&'static str),
    /// Hour, 12-hour when the template also has `%pm`.
    Hour,
    /// Zero-padded to this many digits.
    Counter(usize),
    Process,
    Title,
    Width,
    Height,
    User,
    Computer,
}

/// What each `%` token becomes; `Random` and `Counter` take a `{n}` suffix.
const TOKENS: &[(&str, Part)] = &[
    ("rna", Part::Random(1, UNAMBIGUOUS)),
    ("ra", Part::Random(1, ALPHANUMERIC)),
    ("rn", Part::Random(1, DIGITS)),
    ("rx", Part::Random(1, HEX)),
    ("guid", Part::Guid),
    ("i", Part::Counter(0)),
    ("pn", Part::Process),
    ("t", Part::Title),
    ("width", Part::Width),
    ("height", Part::Height),
    ("un", Part::User),
    ("cn", Part::Computer),
    ("y", Part::Time("%Y")),
    ("yy", Part::Time("%y")),
    ("mo", Part::Time("%m")),
    ("mon", Part::Time("%B")),
    ("mon2", Part::Time("%b")),
    ("d", Part::Time("%d")),
    ("h", Part::Hour),
    ("mi", Part::Time("%M")),
    ("s", Part::Time("%S")),
    ("ms", Part::Time("%3f")),
    ("pm", Part::Time("%p")),
    ("w", Part::Time("%A")),
    ("w2", Part::Time("%a")),
    ("wy", Part::Time("%V")),
    ("unix", Part::Time("%s")),
];

#[derive(Debug, Clone, PartialEq)]
pub struct Template {
    parts: Vec<Part>,
    twelve_hour: bool,
}

impl FromStr for Template {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let mut parts = Vec::new();
        let mut text = String::new();
        let mut rest = s;
        while let Some(at) = rest.find('%') {
            text.push_str(&rest[..at]);
            let after = &rest[at + 1..];
            // Longest match, so %rna isn't read as %rn followed by "a".
            let Some((name, part)) = TOKENS
                .iter()
                .filter(|(n, _)| after.starts_with(n))
                .max_by_key(|(n, _)| n.len())
            else {
                // Like ShareX, an unknown token is kept as text.
                text.push('%');
                rest = after;
                continue;
            };
            rest = &after[name.len()..];
            let mut part = part.clone();
            if let Part::Random(n, _) | Part::Counter(n) = &mut part {
                if let Some((count, tail)) = repeat_count(rest) {
                    if !(1..=MAX_REPEAT).contains(&count) {
                        return Err(format!("%{name}{{{count}}}: count must be 1-{MAX_REPEAT}"));
                    }
                    *n = count;
                    rest = tail;
                }
            }
            if !text.is_empty() {
                parts.push(Part::Text(std::mem::take(&mut text)));
            }
            parts.push(part);
        }
        text.push_str(rest);
        if !text.is_empty() {
            parts.push(Part::Text(text));
        }
        let twelve_hour = parts.contains(&Part::Time("%p"));
        Ok(Self { parts, twelve_hour })
    }
}

/// Splits a leading `{n}` off `s`.
fn repeat_count(s: &str) -> Option<(usize, &str)> {
    let (inside, tail) = s.strip_prefix('{')?.split_once('}')?;
    Some((inside.parse().ok()?, tail))
}

/// Converts a template from the old `{random}`-style syntax, so existing
/// settings keep working. Templates without `{...}` come back unchanged.
pub fn migrate_legacy(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('{') {
        let Some(len) = rest[start..].find('}') else {
            break;
        };
        let end = start + len;
        out.push_str(&rest[..start]);
        let token = &rest[start + 1..end];
        let (name, arg) = token
            .split_once(':')
            .map_or((token, None), |(n, a)| (n, Some(a)));
        let converted = match (name, arg) {
            ("random", n) => format!("%rna{{{}}}", n.unwrap_or("10")),
            ("random_number", n) => format!("%rn{{{}}}", n.unwrap_or("6")),
            ("random_hex", n) => format!("%rx{{{}}}", n.unwrap_or("8")),
            ("counter", Some(n)) => format!("%i{{{n}}}"),
            ("date" | "time", Some(fmt)) => strftime_to_tokens(fmt),
            _ => match name {
                "guid" => "%guid",
                "counter" => "%i",
                "process" => "%pn",
                "title" => "%t",
                "width" => "%width",
                "height" => "%height",
                "user" => "%un",
                "computer" => "%cn",
                "date" => "%y-%mo",
                "time" => "%h-%mi-%s",
                "year" => "%y",
                "yy" => "%yy",
                "month" => "%mo",
                "month_name" => "%mon",
                "week" => "%wy",
                "weekday" => "%w",
                "day" => "%d",
                "hour" => "%h",
                "minute" => "%mi",
                "second" => "%s",
                "ms" => "%ms",
                "ampm" => "%pm",
                "unix" => "%unix",
                // Not an old placeholder, e.g. the {4} of %i{4}.
                _ => &rest[start..=end],
            }
            .to_string(),
        };
        out.push_str(&converted);
        rest = &rest[end + 1..];
    }
    out + rest
}

/// Maps the strftime codes of the old `{date:FMT}` onto tokens.
fn strftime_to_tokens(fmt: &str) -> String {
    let mut out = String::new();
    let mut chars = fmt.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        out.push_str(match chars.next() {
            Some('Y') => "%y",
            Some('y') => "%yy",
            Some('m') => "%mo",
            Some('B') => "%mon",
            Some('b') => "%mon2",
            Some('d') => "%d",
            Some('H') => "%h",
            Some('M') => "%mi",
            Some('S') => "%s",
            Some('p') => "%pm",
            Some('A') => "%w",
            Some('a') => "%w2",
            Some('V') => "%wy",
            Some('s') => "%unix",
            _ => "",
        });
    }
    out
}

fn random(len: usize, charset: &[u8]) -> String {
    (0..len)
        .map(|_| charset[fastrand::usize(..charset.len())] as char)
        .collect()
}

impl Template {
    fn has_random(&self) -> bool {
        self.parts
            .iter()
            .any(|p| matches!(p, Part::Random(..) | Part::Guid))
    }

    pub fn uses_counter(&self) -> bool {
        self.parts.iter().any(|p| matches!(p, Part::Counter(_)))
    }

    /// Renders an object key for uploads: safe `/`-separated segments plus
    /// `.{ext}` (nothing if `ext` is empty), never empty.
    pub fn render_key(&self, ctx: &Context, ext: &str) -> String {
        let rendered = self.render(ctx);
        let mut key = components(&rendered).collect::<Vec<_>>().join("/");
        if key.is_empty() {
            key = "screenshot".into();
        }
        if !ext.is_empty() {
            key = format!("{key}.{ext}");
        }
        key
    }

    /// Renders the template. `/` (or `\`) in literal text separates folders;
    /// substituted values can never introduce separators.
    fn render(&self, ctx: &Context) -> String {
        let mut out = String::new();
        for part in &self.parts {
            let value = match part {
                Part::Text(t) => {
                    out.push_str(t);
                    continue;
                }
                Part::Time(fmt) => ctx.time.format(fmt).to_string(),
                Part::Hour => ctx
                    .time
                    .format(if self.twelve_hour { "%I" } else { "%H" })
                    .to_string(),
                Part::Random(len, charset) => random(*len, charset),
                Part::Guid => {
                    let h = random(32, HEX);
                    format!(
                        "{}-{}-{}-{}-{}",
                        &h[..8],
                        &h[8..12],
                        &h[12..16],
                        &h[16..20],
                        &h[20..]
                    )
                }
                Part::Counter(width) => format!("{:0width$}", ctx.counter, width = *width),
                Part::Process => ctx.process.clone().unwrap_or_else(|| "unknown".into()),
                Part::Title => ctx
                    .title
                    .as_deref()
                    .unwrap_or("untitled")
                    .chars()
                    .take(MAX_TITLE_CHARS)
                    .collect(),
                Part::Width => ctx.width.to_string(),
                Part::Height => ctx.height.to_string(),
                Part::User => USER.clone(),
                Part::Computer => COMPUTER.clone(),
            };
            out.push_str(&sanitize(&value));
        }
        out
    }
}

/// Replaces characters that aren't allowed in file names.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect()
}

/// Splits a rendered path into safe components, dropping empty, `.` and `..`.
fn components(s: &str) -> impl Iterator<Item = &str> {
    s.split(['/', '\\'])
        // Windows silently strips trailing dots and spaces.
        .map(|c| c.trim().trim_end_matches('.'))
        .filter(|c| !c.is_empty() && *c != "..")
}

pub struct Naming {
    pub dir: PathBuf,
    pub subdir: Template,
    pub name: Template,
}

impl Naming {
    pub fn uses_counter(&self) -> bool {
        self.subdir.uses_counter() || self.name.uses_counter()
    }

    /// An example path for the settings window. Uses a fixed seed so the
    /// random part doesn't change on every repaint.
    pub fn preview(&self, ctx: &Context) -> PathBuf {
        fastrand::seed(0x5eed);
        self.candidate(ctx, None)
    }

    fn candidate(&self, ctx: &Context, suffix: Option<u32>) -> PathBuf {
        let mut path = self.dir.clone();
        path.extend(components(&self.subdir.render(ctx)));
        let name = self.name.render(ctx);
        let mut name = components(&name).collect::<Vec<_>>().join("_");
        if name.is_empty() {
            name = "screenshot".into();
        }
        if let Some(n) = suffix {
            let _ = write!(name, "-{n}");
        }
        path.join(name + ".png")
    }

    /// Picks a path for a new screenshot that doesn't overwrite an existing one.
    pub fn path_for(&self, ctx: &Context) -> PathBuf {
        let retries = if self.name.has_random() { 16 } else { 1 };
        for _ in 0..retries {
            let path = self.candidate(ctx, None);
            if !path.exists() {
                return path;
            }
        }
        (2..)
            .map(|n| self.candidate(ctx, Some(n)))
            .find(|p| !Path::exists(p))
            .expect("ran out of suffixes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ctx() -> Context {
        Context {
            process: Some("my:app".into()),
            title: Some("Docs / Notes".into()),
            width: 640,
            height: 480,
            counter: 7,
            ..Context::new(Local.with_ymd_and_hms(2026, 10, 4, 14, 30, 5).unwrap())
        }
    }

    fn naming(subdir: &str, name: &str) -> Naming {
        Naming {
            dir: "base".into(),
            subdir: subdir.parse().unwrap(),
            name: name.parse().unwrap(),
        }
    }

    fn render(name: &str) -> String {
        naming("", name)
            .candidate(&ctx(), None)
            .file_stem()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned()
    }

    #[test]
    fn random_tokens_repeat() {
        let stem = render("%rna{10}");
        assert_eq!(stem.len(), 10);
        assert!(stem.bytes().all(|b| UNAMBIGUOUS.contains(&b)));
        assert_eq!(UNAMBIGUOUS.len(), 56);
        assert_eq!(render("%rna").len(), 1);
        assert_eq!(render("%rn{5}").len(), 5);
        assert!(render("%rn{5}").bytes().all(|b| b.is_ascii_digit()));
        assert_eq!(render("%ra{3}x").len(), 4);
        assert_eq!(render("%guid").len(), 36);
    }

    #[test]
    fn date_and_time_tokens() {
        assert_eq!(render("%y-%mo"), "2026-10");
        assert_eq!(render("%y%mo%d_%h%mi%s"), "20261004_143005");
        assert_eq!(render("%yy_%mon_%mon2_%w_%w2"), "26_October_Oct_Sunday_Sun");
        assert_eq!(render("%h%pm"), "02PM");
    }

    #[test]
    fn window_image_and_counter_values() {
        assert_eq!(render("%t_%width x%height"), "Docs _ Notes_640 x480");
        assert_eq!(render("%i-%i{4}"), "7-0007");
    }

    #[test]
    fn unknown_tokens_stay_as_text() {
        assert_eq!(render("100%_%q"), "100%_%q");
        assert_eq!(render("%d{3}"), "04{3}");
    }

    #[test]
    fn folders_from_process_and_date() {
        let p = naming("%pn/%y/%mo", "%h-%mi-%s").candidate(&ctx(), None);
        assert_eq!(p, Path::new("base/my_app/2026/10/14-30-05.png"));
    }

    #[test]
    fn values_cannot_escape_base_dir() {
        let mut c = ctx();
        c.process = Some("../../etc".into());
        let p = naming("../%pn", "x").candidate(&c, None);
        assert_eq!(p, Path::new("base/.._.._etc/x.png"));
    }

    #[test]
    fn object_keys_keep_folders() {
        let t: Template = "%y-%mo/../%pn/%h-%mi-%s".parse().unwrap();
        assert_eq!(t.render_key(&ctx(), "png"), "2026-10/my_app/14-30-05.png");
        assert_eq!(
            "".parse::<Template>().unwrap().render_key(&ctx(), "png"),
            "screenshot.png"
        );
        assert_eq!(t.render_key(&ctx(), ""), "2026-10/my_app/14-30-05");
    }

    #[test]
    fn rejects_bad_counts() {
        assert!("%ra{0}".parse::<Template>().is_err());
        assert!("%rn{65}".parse::<Template>().is_err());
    }

    #[test]
    fn migrates_old_templates() {
        assert_eq!(migrate_legacy("{random}"), "%rna{10}");
        assert_eq!(migrate_legacy("{date}/{random:6}"), "%y-%mo/%rna{6}");
        assert_eq!(
            migrate_legacy("{process}/{date:%Y/%m}_{counter:3}"),
            "%pn/%y/%mo_%i{3}"
        );
        assert_eq!(migrate_legacy("plain_%rn{4}"), "plain_%rn{4}");
    }

    #[test]
    fn every_menu_placeholder_parses() {
        for (_, items) in CATEGORIES {
            for (token, _) in *items {
                let t: Template = token.parse().unwrap();
                assert!(
                    t.parts.iter().all(|p| !matches!(p, Part::Text(_))),
                    "{token}"
                );
            }
        }
    }
}
