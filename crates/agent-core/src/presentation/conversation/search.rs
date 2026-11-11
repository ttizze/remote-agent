//! Literal case-insensitive matches in displayed text, in UTF-8 byte ranges.
//! Renderers translate these shared ranges to their own glyph coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextMatch {
    pub start: usize,
    pub end: usize,
}

pub fn find_matches(text: &str, query: &str) -> Vec<TextMatch> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let Ok(pattern) = regex::RegexBuilder::new(&regex::escape(query))
        .case_insensitive(true)
        .build()
    else {
        return Vec::new();
    };
    pattern
        .find_iter(text)
        .map(|found| TextMatch {
            start: found.start(),
            end: found.end(),
        })
        .collect()
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
}
