//! Literal case-insensitive matches in displayed text, in UTF-8 byte ranges.
//! Renderers translate these shared ranges to their own glyph coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextMatch {
    pub start: usize,
    pub end: usize,
}

pub fn step_find_index(index: usize, total: usize, delta: i32) -> usize {
    if total == 0 {
        return 0;
    }
    ((index.min(total - 1) as i128 + i128::from(delta)).rem_euclid(total as i128)) as usize
}

pub fn format_find_count(index: usize, total: usize) -> String {
    if total == 0 {
        "0/0".into()
    } else {
        format!("{}/{total}", index.min(total - 1) + 1)
    }
}

pub fn find_matches(text: &str, query: &str) -> Vec<TextMatch> {
    let query = crate::text::trim(query);
    if query.is_empty() {
        return Vec::new();
    }
    let (text, positions) = folded(text);
    let (query, _) = folded(query);
    text.match_indices(&query)
        .map(|(start, _)| TextMatch {
            start: positions[start],
            end: positions[start + query.len()],
        })
        .collect()
}

// Pinned shared/threadSearch.ts preserves UTF-16 length when lowercasing,
// folds the final sigma and keyboard apostrophes, and uses literal matches.
// Native consumers need positions in the original UTF-8 text, including when
// a length-preserving Unicode lowercase changes its UTF-8 byte length.
fn folded(text: &str) -> (String, Vec<usize>) {
    let mut folded = String::new();
    let mut positions = Vec::new();
    for (at, character) in text.char_indices() {
        let lowercase: String = character.to_lowercase().collect();
        if lowercase.encode_utf16().count() == character.len_utf16() {
            folded.extend(lowercase.chars().map(|character| match character {
                'ς' => 'σ',
                '‘' | '’' => '\'',
                other => other,
            }));
        } else {
            folded.push(character);
        }
        positions.resize(folded.len(), at);
    }
    positions.push(text.len());
    (folded, positions)
}

/// Return the wrapped adjacent occurrence index, or `None` when there are no matches.
pub fn adjacent_match_index(current: u64, count: u64, forward: bool) -> Option<u64> {
    if count == 0 {
        return None;
    }
    let current = current % count;
    Some(if forward {
        if current == count - 1 { 0 } else { current + 1 }
    } else if current == 0 {
        count - 1
    } else {
        current - 1
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_are_literal_nonoverlapping_and_ignore_case_and_outer_query_space() {
        let text = "Alpha alpha [.*]";
        assert_eq!(
            find_matches(text, " ALPHA "),
            [
                TextMatch { start: 0, end: 5 },
                TextMatch { start: 6, end: 11 }
            ]
        );
        assert_eq!(
            find_matches(text, "[.*]"),
            [TextMatch { start: 12, end: 16 }]
        );
        assert_eq!(
            find_matches("aaaa", "aa"),
            [
                TextMatch { start: 0, end: 2 },
                TextMatch { start: 2, end: 4 }
            ]
        );
        assert!(find_matches(text, "  ").is_empty());
        assert!(find_matches(text, "absent").is_empty());
    }

    #[test]
    fn unicode_case_insensitive_ranges_refer_to_original_bytes() {
        let text = "😀É é Σς";
        for (query, expected) in [
            ("é", vec!["É", "é"]),
            ("σ", vec!["Σ", "ς"]),
            ("😀", vec!["😀"]),
        ] {
            let actual: Vec<_> = find_matches(text, query)
                .into_iter()
                .map(|range| &text[range.start..range.end])
                .collect();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn adjacent_match_navigation_wraps_and_handles_an_empty_result() {
        assert_eq!(adjacent_match_index(0, 0, true), None);
        assert_eq!(adjacent_match_index(0, 1, true), Some(0));
        assert_eq!(adjacent_match_index(2, 3, true), Some(0));
        assert_eq!(adjacent_match_index(0, 3, false), Some(2));
        assert_eq!(adjacent_match_index(2, 3, false), Some(1));
        assert_eq!(adjacent_match_index(4, 3, true), Some(2));
    }
}
