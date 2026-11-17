//! ECMAScript text boundaries used by the pinned T3 presentation helpers.
pub(crate) const SPACE: &str = concat!(
    r"[\t\n\x0B\x0C\r \xA0\x{1680}\x{2000}-\x{200A}",
    r"\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]",
);

pub(crate) fn is_space(character: char) -> bool {
    character == '\u{feff}' || (character.is_whitespace() && character != '\u{0085}')
}

pub(crate) fn trim(text: &str) -> &str {
    text.trim_matches(is_space)
}

pub(crate) fn trim_end(text: &str) -> &str {
    text.trim_end_matches(is_space)
}

/// ECMAScript's `replace(/\s+/g, " ")`, without trimming either boundary.
pub(crate) fn collapse_spaces(text: &str) -> String {
    let mut result = String::new();
    let mut previous_space = false;
    for character in text.chars() {
        let space = is_space(character);
        if !space || !previous_space {
            result.push(if space { ' ' } else { character });
        }
        previous_space = space;
    }
    result
}
