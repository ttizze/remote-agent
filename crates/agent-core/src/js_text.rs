//! JavaScript string semantics the ported presentation keeps: `\s` and
//! `trim()` whitespace, UTF-16 lengths and offsets, and `decodeURIComponent`.

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
