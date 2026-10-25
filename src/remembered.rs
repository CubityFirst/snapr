//! The capture tools' last settings (colour, size, each tool's options and
//! the magnifier's zoom), kept between runs in tools.toml.

use serde::{Deserialize, Serialize};

use crate::annotate::{
    BENDS, HighlighterInk, HighlighterShape, STEP_SIZES, Size, Style, TEXT_SIZES,
};
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
    /// Nodes along lines and arrows to bend them by.
    bends: u8,
    clip_shadow: bool,
    step: Step,
    /// "freehand", "straight" or "box".
    highlighter: String,
    /// "realistic" or "tint".
    highlighter_ink: String,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Step {
    /// Across the circle, in pixels.
    size: u16,
    /// How tall the digits are, in pixels.
    number: u16,
    border: bool,
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

impl Default for Step {
    fn default() -> Self {
        Remembered::default().step
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
            bends: style.bends,
            clip_shadow: style.clip_shadow,
            step: Step {
                size: style.step.size,
                number: style.step.number,
                border: style.step.border,
            },
            highlighter: match style.highlighter {
                HighlighterShape::Freehand => "freehand",
                HighlighterShape::Straight => "straight",
                HighlighterShape::Box => "box",
            }
            .into(),
            highlighter_ink: match style.highlighter_ink {
                HighlighterInk::Realistic => "realistic",
                HighlighterInk::Tint => "tint",
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
        style.bends = self.bends.clamp(*BENDS.start(), *BENDS.end());
        style.clip_shadow = self.clip_shadow;
        style.step.size = self.step.size.clamp(*STEP_SIZES.start(), *STEP_SIZES.end());
        let numbers = style.step.number_sizes();
        style.step.number = self.step.number.clamp(*numbers.start(), *numbers.end());
        style.step.border = self.step.border;
        style.highlighter = match self.highlighter.as_str() {
            "straight" => HighlighterShape::Straight,
            "box" => HighlighterShape::Box,
            _ => HighlighterShape::Freehand,
        };
        style.highlighter_ink = match self.highlighter_ink.as_str() {
            "tint" => HighlighterInk::Tint,
            _ => HighlighterInk::Realistic,
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
    use crate::annotate::{Size, StepOptions, TextOptions};

    #[test]
    fn round_trips_through_toml() {
        let mut style = Style::default();
        style.color = [1, 2, 3];
        style.size = Size::Large;
        style.pixelate.secure = true;
        style.arrow.double = true;
        style.bends = 3;
        style.clip_shadow = false;
        style.step = StepOptions {
            size: 48,
            number: 24,
            border: false,
        };
        style.highlighter = HighlighterShape::Box;
        style.highlighter_ink = HighlighterInk::Tint;
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
        assert_eq!(style.step, Style::default().step);
        assert_eq!(style.bends, 1);

        assert_eq!(loaded.magnifier(), MAGNIFIER_ZOOM.1 as u32);
        assert_eq!(loaded.font(), Some("Arial"));

        // A number too big for its circle is brought down to fit.
        let crowded: Remembered = toml::from_str("[step]\nsize = 20\nnumber = 90").unwrap();
        assert_eq!(crowded.style().step.number, 11);
    }
}
