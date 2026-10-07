//! Model picker search: every query token must match a model's name, short
//! name, sub-provider, driver or instance name; lower scores rank first.
use crate::js_text::utf16_len;
use crate::view::search_ranking::{QueryMatch, normalize_search_query, score_query_match};

/// Trimmed and lowercased.
fn normalize(input: &str) -> String {
    normalize_search_query(input, |_| false)
}

/// Exact, then prefix, then word boundary, then substring, then (for tokens
/// of three or more characters) subsequence matches, each tier offset from
/// `base`. Both inputs must be normalized.
fn score_token(value: &str, query: &str, base: i64) -> Option<i64> {
    score_query_match(&QueryMatch {
        prefix_base: Some(base + 2),
        boundary_base: Some(base + 4),
        includes_base: Some(base + 6),
        fuzzy_base: (utf16_len(query) >= 3).then_some(base + 100),
        ..QueryMatch::exact(value, query, base)
    })
}

/// The fields a picker row is searched by.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchableModel<'a> {
    /// Indexed so "codex" still matches a custom Codex instance.
    pub driver_kind: &'a str,
    pub provider_display_name: &'a str,
    pub name: &'a str,
    pub short_name: Option<&'a str>,
    pub sub_provider: Option<&'a str>,
    pub is_favorite: bool,
}

const FAVORITE_SCORE_BOOST: i64 = 24;

pub fn build_model_picker_search_text(model: &SearchableModel) -> String {
    normalize(
        &[
            Some(model.name),
            model.short_name,
            model.sub_provider,
            Some(model.driver_kind),
            Some(model.provider_display_name),
        ]
        .into_iter()
        .flatten()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(" "),
    )
}

/// `None` when any token matches no field; favorites rank slightly higher.
pub fn score_model_picker_search<'a>(model: &SearchableModel<'a>, query: &str) -> Option<i64> {
    let query = normalize(query);
    let tokens: Vec<&str> = query.split_whitespace().collect();
    if tokens.is_empty() {
        return Some(0);
    }
    let present = |value: Option<&'a str>| value.filter(|value| !value.is_empty());
    let fields: Vec<String> = [
        Some(model.name),
        present(model.short_name),
        present(model.sub_provider),
    ]
    .into_iter()
    .flatten()
    .map(normalize)
    .chain([
        normalize(model.driver_kind),
        normalize(model.provider_display_name),
        build_model_picker_search_text(model),
    ])
    .collect();
    let mut score = 0;
    for token in tokens {
        score += fields
            .iter()
            .enumerate()
            .filter_map(|(index, field)| score_token(field, token, index as i64 * 10))
            .min()?;
    }
    Some(if model.is_favorite {
        score - FAVORITE_SCORE_BOOST
    } else {
        score
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn copilot_opus() -> SearchableModel<'static> {
        SearchableModel {
            driver_kind: "opencode",
            provider_display_name: "opencode",
            name: "Claude Opus 4.7",
            sub_provider: Some("GitHub Copilot"),
            ..Default::default()
        }
    }

    #[test]
    fn builds_provider_agnostic_search_text_from_generic_fields() {
        assert_eq!(
            build_model_picker_search_text(&copilot_opus()),
            "claude opus 4.7 github copilot opencode opencode"
        );
    }

    #[test]
    fn matches_typo_tolerant_multi_token_queries() {
        assert!(score_model_picker_search(&copilot_opus(), "coplt op").is_some());
    }

    #[test]
    fn rejects_results_when_any_query_token_does_not_match() {
        let codex = SearchableModel {
            driver_kind: "codex",
            provider_display_name: "codex",
            name: "GPT-5 Codex",
            ..Default::default()
        };
        assert!(score_model_picker_search(&codex, "coplt op").is_none());
    }

    #[test]
    fn ranks_exact_token_matches_ahead_of_fuzzier_matches() {
        let exact = score_model_picker_search(&copilot_opus(), "copilot opus").unwrap();
        let fuzzy = score_model_picker_search(&copilot_opus(), "coplt op").unwrap();
        assert!(exact < fuzzy);
    }

    #[test]
    fn gives_favorite_models_a_strong_enough_ranking_boost_for_partial_queries() {
        let favorite = SearchableModel {
            driver_kind: "claudeAgent",
            provider_display_name: "Claude",
            name: "Claude Opus 4.7",
            is_favorite: true,
            ..Default::default()
        };
        let other = SearchableModel {
            driver_kind: "cursor",
            provider_display_name: "Cursor",
            name: "Opus 4.5",
            ..Default::default()
        };
        assert!(
            score_model_picker_search(&favorite, "opu").unwrap()
                < score_model_picker_search(&other, "opu").unwrap()
        );
    }

    #[test]
    fn does_not_let_the_favorite_boost_outrank_clearly_better_textual_matches() {
        let favorite = SearchableModel {
            driver_kind: "claudeAgent",
            provider_display_name: "Claude",
            name: "Claude Opus 4.7",
            is_favorite: true,
            ..Default::default()
        };
        let exact = SearchableModel {
            driver_kind: "cursor",
            provider_display_name: "Cursor",
            name: "Opus 4.7",
            ..Default::default()
        };
        assert!(
            score_model_picker_search(&exact, "opus 4.7").unwrap()
                < score_model_picker_search(&favorite, "opus 4.7").unwrap()
        );
    }

    #[test]
    fn matches_a_custom_instance_display_name_against_its_models() {
        let model = SearchableModel {
            driver_kind: "codex",
            provider_display_name: "Codex Personal",
            name: "GPT-5 Codex",
            ..Default::default()
        };
        assert!(score_model_picker_search(&model, "personal").is_some());
    }
}
