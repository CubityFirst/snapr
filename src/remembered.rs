//! The capture tools' last settings (colour, size, each tool's options and
//! the magnifier's zoom), kept between runs in tools.toml.

use serde::{Deserialize, Serialize};

use crate::annotate::{HighlighterShape, Size, Style, TEXT_SIZES};
use crate::session::{MAGNIFIER_PIXELS, MAGNIFIER_ZOOM};
use crate::settings::Settings;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Remembered {
    color: [u8; 3],
    /// "small", "medium" or "large".
    size: String,
    pixelate_block: u32,
    pixelate_secure: bool,
    arrow_double: bool,
    arrow_head_at_start: bool,
    clip_shadow: bool,
    /// "freehand", "straight" or "box".
    highlighter: String,
    /// Pixels across the magnifier; 0 when it's hidden.
    magnifier: u32,
    text: Text,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Text {
    /// An installed font's family name; empty for the bundled one.
    font: String,
    size: u16,
    outline: bool,
}

impl Default for Remembered {
    fn default() -> Self {
        Self::new(Style::default(), MAGNIFIER_PIXELS)
    }
}

impl Default for Text {
    fn default() -> Self {
        Remembered::default().text
    }
}

fn file() -> Option<std::path::PathBuf> {
    Some(Settings::config_dir()?.join("tools.toml"))
}

impl Remembered {
    pub fn new(style: Style, magnifier: u32) -> Self {
        Self {
            color: style.color,
            size: match style.size {
                Size::Small => "small",
                Size::Medium => "medium",
                Size::Large => "large",
            }
            .into(),
            pixelate_block: style.pixelate.block,
            pixelate_secure: style.pixelate.secure,
            arrow_double: style.arrow.double,
            arrow_head_at_start: style.arrow.head_at_start,
            clip_shadow: style.clip_shadow,
            highlighter: match style.highlighter {
                HighlighterShape::Freehand => "freehand",
                HighlighterShape::Straight => "straight",
                HighlighterShape::Box => "box",
            }
            .into(),
            magnifier,
            text: Text {
                font: style
                    .text
                    .font
                    .and_then(crate::fonts::family)
                    .unwrap_or_default()
                    .into(),
                size: style.text.size,
                outline: style.text.outline,
            },
        }
    }

    /// The saved settings, or the defaults.
    pub fn load() -> Self {
        let Some(text) = file().and_then(|f| std::fs::read_to_string(f).ok()) else {
            return Self::default();
        };
        toml::from_str(&text).unwrap_or_else(|e| {
            eprintln!("couldn't read the tools' settings: {e}");
            Self::default()
        })
    }

    pub fn save(&self) {
        let Some(path) = file() else { return };
        let result = toml::to_string(self)
            .map_err(|e| e.to_string())
            .and_then(|text| crate::atomic::write(&path, text).map_err(|e| e.to_string()));
        if let Err(e) = result {
            eprintln!("couldn't save the tools' settings: {e}");
        }
    }

    /// The style, without its font: that's looked up with [`Self::font`]
    /// once the installed fonts have been found.
    pub fn style(&self) -> Style {
        let mut style = Style::default();
        style.color = self.color;
        style.size = match self.size.as_str() {
            "small" => Size::Small,
            "large" => Size::Large,
            _ => Size::Medium,
        };
        if self.pixelate_block > 0 {
            style.pixelate.block = self.pixelate_block.min(400);
        }
        style.pixelate.secure = self.pixelate_secure;
        style.arrow.double = self.arrow_double;
        style.arrow.head_at_start = self.arrow_head_at_start;
        style.clip_shadow = self.clip_shadow;
        style.highlighter = match self.highlighter.as_str() {
            "straight" => HighlighterShape::Straight,
            "box" => HighlighterShape::Box,
            _ => HighlighterShape::Freehand,
        };
        style.text.size = self.text.size.clamp(*TEXT_SIZES.start(), *TEXT_SIZES.end());
        style.text.outline = self.text.outline;
        style
    }

    /// The text tool's font family, if it isn't the bundled one.
    pub fn font(&self) -> Option<&str> {
        Some(self.text.font.as_str()).filter(|f| !f.is_empty())
    }

    pub fn magnifier(&self) -> u32 {
        let (min, max) = MAGNIFIER_ZOOM;
        match self.magnifier {
            0 => 0,
            // Odd, so one pixel is in the middle.
            n => (n as i32).clamp(min, max) as u32 | 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotate::{Size, TextOptions};

    #[test]
    fn round_trips_through_toml() {
        let mut style = Style::default();
        style.color = [1, 2, 3];
        style.size = Size::Large;
        style.pixelate.secure = true;
        style.arrow.double = true;
        style.clip_shadow = false;
        style.highlighter = HighlighterShape::Box;
        style.text = TextOptions {
            font: None,
            size: 64,
            outline: false,
        };
        let saved = Remembered::new(style, 0);
        let text = toml::to_string(&saved).unwrap();
        let loaded: Remembered = toml::from_str(&text).unwrap();
        assert_eq!(loaded.style(), style);
        assert_eq!(loaded.magnifier(), 0);
        assert_eq!(loaded.font(), None);
    }

    #[test]
    fn bad_or_missing_values_fall_back() {
        let loaded: Remembered =
            toml::from_str("size = \"huge\"\nmagnifier = 400\n[text]\nsize = 2\nfont = \"Arial\"")
                .unwrap();
        let style = loaded.style();
        assert_eq!(style.size, Size::Medium);
        assert_eq!(style.color, Style::default().color);
        assert_eq!(style.text.size, *TEXT_SIZES.start());
        assert!(style.text.outline);
        assert_eq!(loaded.magnifier(), MAGNIFIER_ZOOM.1 as u32);
        assert_eq!(loaded.font(), Some("Arial"));
    }
}
