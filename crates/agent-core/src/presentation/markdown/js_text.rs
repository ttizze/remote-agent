//! JavaScript string semantics the Markdown helpers keep: `\s`/`trim()` whitespace,
//! UTF-16 lengths and offsets, and `decodeURIComponent`.

/// JavaScript `\s`: Unicode White_Space without U+0085, plus U+FEFF.
pub(super) fn js_space(c: char) -> bool {
    c == '\u{feff}' || (c != '\u{85}' && c.is_whitespace())
}

pub(super) fn js_trim(text: &str) -> &str {
    text.trim_matches(js_space)
}

pub(super) fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// The UTF-16 offset of a byte offset that lies on a character boundary.
pub(super) fn utf16_offset(text: &str, byte: usize) -> u64 {
    utf16_len(&text[..byte]) as u64
}

/// The text after the first `units` UTF-16 code units; a cut inside a surrogate pair
/// drops that character.
pub(super) fn utf16_skip(text: &str, units: usize) -> &str {
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
pub(super) fn decode_uri_component(text: &str) -> Option<String> {
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
