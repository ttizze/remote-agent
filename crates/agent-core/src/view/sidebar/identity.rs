//! The generated badge a project without an icon shows: a two-letter
//! monogram tinted with a color picked from its name.
use regex::Regex;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ProjectIconColor {
    Gray,
    Red,
    Orange,
    Amber,
    Yellow,
    Lime,
    Green,
    Emerald,
    Teal,
    Cyan,
    Sky,
    Blue,
    Indigo,
    Violet,
    Purple,
    Fuchsia,
    Pink,
    Rose,
}

const COLORS: [ProjectIconColor; 18] = [
    ProjectIconColor::Gray,
    ProjectIconColor::Red,
    ProjectIconColor::Orange,
    ProjectIconColor::Amber,
    ProjectIconColor::Yellow,
    ProjectIconColor::Lime,
    ProjectIconColor::Green,
    ProjectIconColor::Emerald,
    ProjectIconColor::Teal,
    ProjectIconColor::Cyan,
    ProjectIconColor::Sky,
    ProjectIconColor::Blue,
    ProjectIconColor::Indigo,
    ProjectIconColor::Violet,
    ProjectIconColor::Purple,
    ProjectIconColor::Fuchsia,
    ProjectIconColor::Pink,
    ProjectIconColor::Rose,
];

impl ProjectIconColor {
    /// The glyph color as `#rrggbb`: the 600 shade in light mode, 400 in dark.
    pub fn hex(self, dark: bool) -> &'static str {
        let (light, dark_hex) = match self {
            Self::Gray => ("#4b5563", "#9ca3af"),
            Self::Red => ("#dc2626", "#f87171"),
            Self::Orange => ("#ea580c", "#fb923c"),
            Self::Amber => ("#d97706", "#fbbf24"),
            Self::Yellow => ("#ca8a04", "#facc15"),
            Self::Lime => ("#65a30d", "#a3e635"),
            Self::Green => ("#16a34a", "#4ade80"),
            Self::Emerald => ("#059669", "#34d399"),
            Self::Teal => ("#0d9488", "#2dd4bf"),
            Self::Cyan => ("#0891b2", "#22d3ee"),
            Self::Sky => ("#0284c7", "#38bdf8"),
            Self::Blue => ("#2563eb", "#60a5fa"),
            Self::Indigo => ("#4f46e5", "#818cf8"),
            Self::Violet => ("#7c3aed", "#a78bfa"),
            Self::Purple => ("#9333ea", "#c084fc"),
            Self::Fuchsia => ("#c026d3", "#e879f9"),
            Self::Pink => ("#db2777", "#f472b6"),
            Self::Rose => ("#e11d48", "#fb7185"),
        };
        if dark { dark_hex } else { light }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProjectIdentity {
    pub monogram: String,
    pub color: ProjectIconColor,
}

static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\p{L}\p{N}]+").expect("valid"));

/// The first letter of the first word, then its first digit, else the first
/// letter of the last word, else its last letter; "PR" without any word.
fn monogram(name: &str) -> String {
    let words: Vec<&str> = WORD.find_iter(name.trim()).map(|m| m.as_str()).collect();
    let Some(first_word) = words.first() else {
        return "PR".into();
    };
    let glyphs: Vec<char> = first_word.chars().collect();
    let first = glyphs[0];
    let second = glyphs[1..]
        .iter()
        .copied()
        .find(|glyph| glyph.is_numeric())
        .or_else(|| {
            if words.len() > 1 {
                words.last().and_then(|word| word.chars().next())
            } else {
                glyphs.last().copied()
            }
        })
        .unwrap_or(first);
    format!("{first}{second}")
        .to_uppercase()
        .chars()
        .take(2)
        .collect()
}

fn color(name: &str) -> ProjectIconColor {
    let seed = name.trim().to_lowercase();
    let seed = if seed.is_empty() {
        "project".into()
    } else {
        seed
    };
    let index = seed.chars().fold(0usize, |index, glyph| {
        (index * 31 + glyph as usize) % COLORS.len()
    });
    COLORS[index]
}

pub fn project_identity(name: &str) -> ProjectIdentity {
    ProjectIdentity {
        monogram: monogram(name),
        color: color(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_monogram_reads_the_first_and_last_words() {
        assert_eq!(monogram("remote agent"), "RA");
        assert_eq!(monogram("web"), "WB");
        assert_eq!(monogram("app2 server"), "A2");
        assert_eq!(monogram("x"), "XX");
        assert_eq!(monogram("  --  "), "PR");
    }

    #[test]
    fn the_color_is_stable_for_a_name_and_ignores_case() {
        assert_eq!(color("Docs"), color("docs"));
        assert_eq!(color(""), color("project"));
        // "a" is code point 97, and 97 % 18 picks the eighth color.
        assert_eq!(color("a"), ProjectIconColor::Emerald);
    }

    #[test]
    fn colors_differ_between_modes() {
        for color in COLORS {
            assert_ne!(color.hex(false), color.hex(true));
        }
    }
}
