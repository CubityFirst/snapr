//! Names for colours, from Name that Color's list (`assets/color-names.txt`,
//! credited on the About tab): a colour gets the name of the listed colour
//! that looks closest to it, measured in OKLab.

use std::sync::LazyLock;

const LIST: &str = include_str!("../assets/color-names.txt");

/// The listed colours in OKLab, with their names.
static NAMES: LazyLock<Vec<([f32; 3], &'static str)>> = LazyLock::new(|| {
    LIST.lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let (hex, name) = l.split_once(' ')?;
            let v = u32::from_str_radix(hex, 16).ok()?;
            Some((oklab([(v >> 16) as u8, (v >> 8) as u8, v as u8]), name))
        })
        .collect()
});

/// The list's credits and the licence notices it comes with: the comment at
/// the top of the file.
pub fn notice() -> String {
    LIST.lines()
        .take_while(|l| l.starts_with('#'))
        .map(|l| l.trim_start_matches('#').strip_prefix(' ').unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The name of the listed colour closest to `rgb`.
pub fn name(rgb: [u8; 3]) -> &'static str {
    let [l, a, b] = oklab(rgb);
    NAMES
        .iter()
        .map(|([l2, a2, b2], name)| ((l - l2).powi(2) + (a - a2).powi(2) + (b - b2).powi(2), *name))
        .min_by(|x, y| x.0.total_cmp(&y.0))
        .map_or("", |(_, name)| name)
}

/// An sRGB colour in OKLab, where equal distances look about equally
/// different (https://bottosson.github.io/posts/oklab/), with lightness
/// through Ottosson's "toe" (from his Okhsl post), without which dark
/// colours are too far apart: #010101 came out nearer Jaguar, a purple
/// black, than Black.
fn oklab(rgb: [u8; 3]) -> [f32; 3] {
    let [r, g, b] = rgb.map(|c| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    });
    let l = (0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_99 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    let lightness = 0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s;
    const K1: f32 = 0.206;
    const K2: f32 = 0.03;
    const K3: f32 = (1.0 + K1) / (1.0 + K2);
    let x = K3 * lightness - K1;
    [
        0.5 * (x + (x * x + 4.0 * K2 * K3 * lightness).sqrt()),
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(NAMES.len(), 1566);
        // Listed colours are named exactly.
        assert_eq!(name([0xff, 0xff, 0xff]), "White");
        assert_eq!(name([0, 0, 0]), "Black");
        assert_eq!(name([0xff, 0xfd, 0xd0]), "Cream");
        // Others get the closest.
        assert_eq!(name([0xfe, 0, 0]), "Red");
        assert_eq!(name([1, 1, 1]), "Black");
    }

    #[test]
    fn oklab_white_and_black() {
        let [l, a, b] = oklab([255, 255, 255]);
        assert!((l - 1.0).abs() < 1e-3 && a.abs() < 1e-3 && b.abs() < 1e-3);
        assert!(oklab([0, 0, 0]).iter().all(|v| v.abs() < 1e-6));
    }
}
