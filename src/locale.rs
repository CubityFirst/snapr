//! Spelling for where snapr runs. Its text is written in British English;
//! with the system set to US English it says "color" and "canceled".

use std::borrow::Cow;
use std::sync::LazyLock;

/// The British spellings in snapr's text and their American ones.
const AMERICAN: &[(&str, &str)] = &[
    ("Colour", "Color"),
    ("colour", "color"),
    ("cancelled", "canceled"),
];

static US: LazyLock<bool> = LazyLock::new(|| {
    // Tests check text in the spelling it's written in.
    !cfg!(test) && system_locale().is_some_and(|l| is_us(&l))
});

/// `text`, spelled for the system's locale.
pub fn spell(text: &str) -> Cow<'_, str> {
    spell_for(text, *US)
}

fn spell_for(text: &str, us: bool) -> Cow<'_, str> {
    if !us || !AMERICAN.iter().any(|(gb, _)| text.contains(gb)) {
        return Cow::Borrowed(text);
    }
    let mut text = text.to_owned();
    for (gb, us) in AMERICAN {
        text = text.replace(gb, us);
    }
    Cow::Owned(text)
}

/// A string literal, spelled for the system's locale, as a `&'static str`
/// (worked out once for each place it's used).
macro_rules! spelled {
    ($text:literal) => {{
        static TEXT: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
        *TEXT.get_or_init(|| match $crate::locale::spell($text) {
            std::borrow::Cow::Borrowed(s) => s,
            std::borrow::Cow::Owned(s) => s.leak(),
        })
    }};
}
pub(crate) use spelled;

/// Whether a locale name (`en-US`, `en_US.UTF-8`, `en-Latn-US`) is English
/// as written in the United States.
fn is_us(locale: &str) -> bool {
    let locale = locale.split(['.', '@']).next().unwrap_or_default();
    let mut parts = locale.split(['-', '_']);
    parts.next().is_some_and(|lang| lang.eq_ignore_ascii_case("en"))
        && parts.any(|p| p.eq_ignore_ascii_case("US"))
}

/// The user's locale, as the OS names it.
#[cfg(windows)]
fn system_locale() -> Option<String> {
    use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;
    // LOCALE_NAME_MAX_LENGTH
    let mut name = [0u16; 85];
    // SAFETY: writes at most `name.len()` characters, NUL included.
    let len = unsafe { GetUserDefaultLocaleName(name.as_mut_ptr(), name.len() as i32) };
    (len > 1).then(|| String::from_utf16_lossy(&name[..len as usize - 1]))
}

#[cfg(target_os = "macos")]
fn system_locale() -> Option<String> {
    Some(objc2_foundation::NSLocale::currentLocale().localeIdentifier().to_string())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn system_locale() -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(|v| std::env::var(v).ok())
        .find(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_us_english() {
        for us in ["en-US", "en_US.UTF-8", "en-Latn-US", "en_us"] {
            assert!(is_us(us), "{us}");
        }
        for other in ["en-GB", "en_AU.UTF-8", "en", "C.UTF-8", "POSIX", "es-US", ""] {
            assert!(!is_us(other), "{other}");
        }
    }

    #[test]
    fn respells_for_the_us() {
        assert_eq!(spell_for("Colours picked", true), "Colors picked");
        assert_eq!(spell_for("Pick a colour", true), "Pick a color");
        assert_eq!(spell_for("Upload to x cancelled", true), "Upload to x canceled");
        assert_eq!(spell_for("Pick a colour", false), "Pick a colour");
        assert!(matches!(spell_for("Blur", true), Cow::Borrowed(_)));
    }
}
