//! JavaScript string semantics the ported labels depend on: `\s`, `trim` and
//! UTF-16 lengths.

/// JavaScript `\s` as a regex character class.
pub(crate) const JS_SPACE: &str = r"[\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]";

/// JavaScript `.` without the `s` flag.
pub(crate) const JS_DOT: &str = r"[^\n\r\x{2028}\x{2029}]";

/// JavaScript `\s`.
pub(crate) fn is_js_space(c: char) -> bool {
    const SPACES: [char; 8] = [
        ' ', '\u{A0}', '\u{1680}', '\u{2028}', '\u{2029}', '\u{202F}', '\u{205F}', '\u{3000}',
    ];
    ('\t'..='\r').contains(&c)
        || ('\u{2000}'..='\u{200A}').contains(&c)
        || c == '\u{FEFF}'
        || SPACES.contains(&c)
}

/// JavaScript `String.prototype.trim`.
pub(crate) fn js_trim(value: &str) -> &str {
    value.trim_matches(is_js_space)
}

pub(crate) fn utf16_len(value: &str) -> usize {
    value.chars().map(char::len_utf16).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_javascript_whitespace_only() {
        assert_eq!(js_trim("\u{FEFF} a \u{3000}"), "a");
        assert_eq!(js_trim("\u{85}a"), "\u{85}a");
        assert_eq!(utf16_len("😄a"), 3);
    }
}
