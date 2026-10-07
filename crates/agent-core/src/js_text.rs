//! JavaScript string and number semantics the ported presentation keeps:
//! `\s` and `trim()` whitespace, UTF-16 lengths and offsets,
//! `decodeURIComponent`, `Number()` and `toFixed`.
use regex::Regex;

/// JavaScript `\s` as a regex character class.
pub(crate) const JS_SPACE: &str = r"[\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]";

/// JavaScript `.` without the `s` flag.
pub(crate) const JS_DOT: &str = r"[^\n\r\x{2028}\x{2029}]";

/// JavaScript `\s`: Unicode White_Space without U+0085, plus U+FEFF.
pub(crate) fn is_js_space(c: char) -> bool {
    c == '\u{feff}' || (c != '\u{85}' && c.is_whitespace())
}

/// JavaScript `String.prototype.trim`.
pub(crate) fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_space)
}

/// JavaScript `String.prototype.trimStart`.
pub(crate) fn js_trim_start(text: &str) -> &str {
    text.trim_start_matches(is_js_space)
}

/// A regex whose `\s` and `\S` mean the JavaScript classes.
pub(crate) fn js_regex(pattern: &str) -> Regex {
    let set = &JS_SPACE[1..JS_SPACE.len() - 1];
    let pattern = pattern
        .replace(r"\S", &format!("[^{set}]"))
        .replace(r"\s", JS_SPACE);
    Regex::new(&pattern).expect("pattern compiles")
}

pub(crate) fn utf16_units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

pub(crate) fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// The longest prefix of at most `units` UTF-16 code units, like `slice(0, units)`
/// except that a surrogate pair is never split.
pub(crate) fn utf16_prefix(text: &str, units: usize) -> &str {
    let mut count = 0;
    for (index, c) in text.char_indices() {
        count += c.len_utf16();
        if count > units {
            return &text[..index];
        }
    }
    text
}

/// The UTF-16 offset of a byte offset that lies on a character boundary.
pub(crate) fn utf16_offset(text: &str, byte: usize) -> u64 {
    utf16_len(&text[..byte]) as u64
}

/// The text after the first `units` UTF-16 code units; a cut inside a surrogate pair
/// drops that character.
pub(crate) fn utf16_skip(text: &str, units: usize) -> &str {
    let mut used = 0;
    for (index, c) in text.char_indices() {
        if used >= units {
            return &text[index..];
        }
        used += c.len_utf16();
    }
    ""
}

/// `decodeURIComponent`: malformed escapes and invalid UTF-8 fail.
pub(crate) fn decode_uri_component(text: &str) -> Option<String> {
    if !text.contains('%') {
        return Some(text.to_owned());
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = |at: usize| (*bytes.get(at)? as char).to_digit(16);
            out.push((hex(index + 1)? * 16 + hex(index + 2)?) as u8);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// JavaScript `Number(value)` for a non-blank string, or `None` when it is NaN.
pub(crate) fn js_number(value: &str) -> Option<f64> {
    let text = js_trim(value);
    let radix = |prefix: [&str; 2], radix: u32| {
        let digits = prefix.iter().find_map(|prefix| text.strip_prefix(prefix))?;
        let valid = !digits.is_empty() && digits.chars().all(|c| c.is_digit(radix));
        Some(valid.then(|| {
            digits.chars().fold(0.0, |total, c| {
                total * f64::from(radix) + f64::from(c.to_digit(radix).unwrap_or(0))
            })
        }))
    };
    for (prefix, base) in [(["0x", "0X"], 16), (["0o", "0O"], 8), (["0b", "0B"], 2)] {
        if let Some(number) = radix(prefix, base) {
            return number;
        }
    }
    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    if unsigned == "Infinity" {
        return Some(f64::INFINITY);
    }
    let (mantissa, exponent) = unsigned
        .split_once(['e', 'E'])
        .map_or((unsigned, None), |(mantissa, exponent)| {
            (mantissa, Some(exponent))
        });
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
    let exponent_valid = exponent.is_none_or(|exponent| {
        let exponent = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
        !exponent.is_empty() && digits(exponent)
    });
    let valid = (!whole.is_empty() || !fraction.is_empty())
        && digits(whole)
        && digits(fraction)
        && exponent_valid;
    valid.then(|| text.parse().ok()).flatten()
}

/// JavaScript `Number.prototype.toFixed` for finite, non-negative values:
/// ties round up.
pub(crate) fn js_to_fixed(value: f64, digits: usize) -> String {
    // 60 places print the exact binary value of these magnitudes.
    let exact = format!("{value:.60}");
    let (integer, fraction) = exact.split_once('.').unwrap_or((&exact, ""));
    let mut kept: Vec<u8> = integer
        .bytes()
        .chain(fraction.bytes().take(digits))
        .collect();
    if fraction
        .as_bytes()
        .get(digits)
        .is_some_and(|digit| *digit >= b'5')
    {
        let mut index = kept.len();
        loop {
            if index == 0 {
                kept.insert(0, b'1');
                break;
            }
            index -= 1;
            if kept[index] == b'9' {
                kept[index] = b'0';
            } else {
                kept[index] += 1;
                break;
            }
        }
    }
    let mut text = String::from_utf8(kept).expect("ASCII digits");
    if digits > 0 {
        text.insert(text.len() - digits, '.');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_javascript_whitespace_only() {
        assert_eq!(js_trim("\u{FEFF} a \u{3000}"), "a");
        assert_eq!(js_trim("\u{85}a"), "\u{85}a");
        assert_eq!(utf16_len("😄a"), 3);
        assert_eq!(utf16_prefix("a😄b", 2), "a");
        assert_eq!(utf16_prefix("a😄b", 3), "a😄");
        assert_eq!(utf16_skip("a😄b", 3), "b");
    }

    #[test]
    fn the_space_class_matches_the_space_predicate() {
        let class = regex::Regex::new(&format!("^{JS_SPACE}$")).unwrap();
        for c in (0..0x3001u32)
            .chain([0xFEFF, 0x85])
            .filter_map(char::from_u32)
        {
            assert_eq!(class.is_match(&c.to_string()), is_js_space(c), "{:?}", c);
        }
    }
}
