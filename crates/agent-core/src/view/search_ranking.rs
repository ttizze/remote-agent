//! Search ranking the pickers share: query normalization, tiered match scores
//! (lower ranks first) and a bounded ranked list.
//!
//! Positions count UTF-16 code units.
use crate::js_text::{js_trim, utf16_len, utf16_offset, utf16_units};

/// Trimmed, without the leading characters `trim_leading` matches, and
/// lowercased.
pub(crate) fn normalize_search_query(input: &str, trim_leading: impl Fn(char) -> bool) -> String {
    js_trim(input)
        .trim_start_matches(trim_leading)
        .to_lowercase()
}

pub(crate) struct QueryMatch<'a> {
    pub value: &'a str,
    pub query: &'a str,
    pub exact_base: i64,
    pub prefix_base: Option<i64>,
    pub boundary_base: Option<i64>,
    pub includes_base: Option<i64>,
    pub fuzzy_base: Option<i64>,
    pub boundary_markers: &'a [&'a str],
}
impl<'a> QueryMatch<'a> {
    pub fn exact(value: &'a str, query: &'a str, exact_base: i64) -> Self {
        Self {
            value,
            query,
            exact_base,
            prefix_base: None,
            boundary_base: None,
            includes_base: None,
            fuzzy_base: None,
            boundary_markers: &[" ", "-", "_", "/"],
        }
    }
}

pub(crate) fn score_subsequence_match(value: &str, query: &str) -> Option<i64> {
    let (value, query) = (utf16_units(value), utf16_units(query));
    if query.is_empty() {
        return Some(0);
    }
    let (mut query_index, mut first, mut previous, mut gap_penalty) = (0, None, None, 0i64);
    for (index, unit) in value.iter().enumerate() {
        if *unit != query[query_index] {
            continue;
        }
        let first = *first.get_or_insert(index);
        if let Some(previous) = previous {
            gap_penalty += (index - previous - 1) as i64;
        }
        previous = Some(index);
        query_index += 1;
        if query_index == query.len() {
            let span_penalty = (index - first + 1 - query.len()) as i64;
            let length_penalty = 64.min(value.len() as i64 - query.len() as i64);
            return Some(first as i64 * 2 + gap_penalty * 3 + span_penalty + length_penalty);
        }
    }
    None
}

fn length_penalty(value: &str, query: &str) -> i64 {
    (utf16_len(value) as i64 - utf16_len(query) as i64).clamp(0, 64)
}

fn utf16_index(text: &str, byte: usize) -> i64 {
    utf16_offset(text, byte) as i64
}

/// Tiered match score, lower is better. Inputs are trimmed and lowercased.
pub(crate) fn score_query_match(input: &QueryMatch<'_>) -> Option<i64> {
    let (value, query) = (input.value, input.query);
    if value.is_empty() || query.is_empty() {
        return None;
    }
    if value == query {
        return Some(input.exact_base);
    }
    if let Some(base) = input.prefix_base
        && value.starts_with(query)
    {
        return Some(base + length_penalty(value, query));
    }
    if let Some(base) = input.boundary_base
        && let Some(index) = input
            .boundary_markers
            .iter()
            .filter_map(|marker| {
                value
                    .find(&format!("{marker}{query}"))
                    .map(|byte| utf16_index(value, byte) + utf16_len(marker) as i64)
            })
            .min()
    {
        return Some(base + index * 2 + length_penalty(value, query));
    }
    if let Some(base) = input.includes_base
        && let Some(byte) = value.find(query)
    {
        return Some(base + utf16_index(value, byte) * 2 + length_penalty(value, query));
    }
    input
        .fuzzy_base
        .and_then(|base| score_subsequence_match(value, query).map(|score| base + score))
}

pub(crate) struct Ranked<T> {
    pub item: T,
    pub score: i64,
    pub tie_breaker: String,
}

/// Keeps `ranked` sorted by score then tie breaker, at most `limit` long;
/// equal entries keep their insertion order.
pub(crate) fn insert_ranked<T>(ranked: &mut Vec<Ranked<T>>, candidate: Ranked<T>, limit: usize) {
    if limit == 0 {
        return;
    }
    let index = ranked.partition_point(|current| {
        (current.score, current.tie_breaker.as_str())
            <= (candidate.score, candidate.tie_breaker.as_str())
    });
    if index >= limit {
        return;
    }
    ranked.insert(index, candidate);
    ranked.truncate(limit);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_and_lowercases_queries() {
        assert_eq!(normalize_search_query("  UI  ", |_| false), "ui");
    }

    #[test]
    fn can_strip_leading_trigger_characters() {
        assert_eq!(normalize_search_query("  $ui", |c| c == '$'), "ui");
    }

    #[test]
    fn prefers_exact_matches_over_broader_contains_matches() {
        assert_eq!(
            score_query_match(&QueryMatch {
                prefix_base: Some(10),
                includes_base: Some(20),
                ..QueryMatch::exact("ui", "ui", 0)
            }),
            Some(0)
        );
        assert!(
            score_query_match(&QueryMatch {
                prefix_base: Some(10),
                boundary_base: Some(20),
                includes_base: Some(30),
                ..QueryMatch::exact("building native ui", "ui", 0)
            })
            .unwrap()
                > 0
        );
    }

    #[test]
    fn treats_boundary_matches_as_stronger_than_generic_contains_matches() {
        let score = |value| {
            score_query_match(&QueryMatch {
                prefix_base: Some(10),
                boundary_base: Some(20),
                includes_base: Some(30),
                boundary_markers: &["-"],
                ..QueryMatch::exact(value, "fix", 0)
            })
            .unwrap()
        };
        assert!(score("gh-fix-ci") < score("highfixci"));
    }

    #[test]
    fn scores_tighter_subsequences_ahead_of_looser_ones() {
        let compact = score_subsequence_match("ghfixci", "gfc").unwrap();
        let spread = score_subsequence_match("github-fix-ci", "gfc").unwrap();
        assert!(compact < spread);
    }

    #[test]
    fn keeps_the_best_ranked_candidates_within_the_limit() {
        let entry = |item: &'static str, score| Ranked {
            item,
            score,
            tie_breaker: item.into(),
        };
        let mut ranked = vec![entry("b", 20), entry("d", 40)];
        insert_ranked(&mut ranked, entry("a", 10), 2);
        insert_ranked(&mut ranked, entry("c", 30), 2);
        let items: Vec<&str> = ranked.iter().map(|entry| entry.item).collect();
        assert_eq!(items, ["a", "b"]);
    }
}
