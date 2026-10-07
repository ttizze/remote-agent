//! `localeCompare` for names, approximating the root collation: white space,
//! then punctuation and symbols, then digits, then letters ignoring case.
use std::cmp::Ordering;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CollationClass {
    Space,
    Punctuation,
    Digit,
    Letter,
}

/// Root-collation rank of the ASCII marks, as `localeCompare` orders them
/// before digits and letters.
const MARKS: &str = "_-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum CollationKey {
    Char(u32),
    /// A digit run compares by value: its length without leading zeros, then digits.
    Number(usize, String),
}

fn collation_elements(text: &str, numeric: bool) -> Vec<(CollationClass, CollationKey)> {
    let mut elements = vec![];
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if numeric && c.is_ascii_digit() {
            let mut digits = String::from(c);
            while let Some(next) = chars.peek().copied().filter(char::is_ascii_digit) {
                digits.push(next);
                chars.next();
            }
            let value = digits.trim_start_matches('0').to_owned();
            elements.push((
                CollationClass::Digit,
                CollationKey::Number(value.len(), value),
            ));
        } else if c.is_ascii_digit() {
            elements.push((CollationClass::Digit, CollationKey::Char(c as u32)));
        } else if c.is_whitespace() {
            elements.push((CollationClass::Space, CollationKey::Char(c as u32)));
        } else if c.is_alphabetic() {
            let lower = c.to_lowercase().next().unwrap_or(c);
            elements.push((CollationClass::Letter, CollationKey::Char(lower as u32)));
        } else {
            let rank = MARKS.find(c).map_or(c as u32, |rank| rank as u32);
            elements.push((CollationClass::Punctuation, CollationKey::Char(rank)));
        }
    }
    elements
}

/// `left.localeCompare(right)`: case breaks ties, lowercase first.
pub(crate) fn locale_compare(left: &str, right: &str) -> Ordering {
    collation_elements(left, false)
        .cmp(&collation_elements(right, false))
        .then_with(|| right.cmp(left))
}

/// `localeCompare` with numeric collation and base sensitivity: case is
/// ignored and digit runs compare by value.
pub(crate) fn numeric_locale_compare(left: &str, right: &str) -> Ordering {
    collation_elements(left, true).cmp(&collation_elements(right, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_names_like_numeric_locale_compare() {
        let mut names = vec![
            "B.txt", "a.txt", "_c.txt", "src-a.ts", "src.ts", "A.txt", "10.txt", "9.txt",
        ];
        names.sort_by(|a, b| numeric_locale_compare(a, b));
        assert_eq!(
            names,
            [
                "_c.txt", "9.txt", "10.txt", "a.txt", "A.txt", "B.txt", "src-a.ts", "src.ts"
            ]
        );
    }

    #[test]
    fn sorts_names_like_locale_compare() {
        let mut names = vec!["B", "b", "a_b", "a-b", "10", "9", "a b", "A"];
        names.sort_by(|a, b| locale_compare(a, b));
        assert_eq!(names, ["10", "9", "A", "a b", "a_b", "a-b", "b", "B"]);
    }
}
