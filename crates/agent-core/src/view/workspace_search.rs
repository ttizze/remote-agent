//! Pure presentation of project content search results.
use crate::state::{ContentSearchQuery, Snapshot};
use agent_protocol::workspace::{ContentMatch, ContentSearch};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentSearchView {
    pub query: Option<ContentSearchQuery>,
    pub matches: Vec<ContentMatch>,
    pub truncated: bool,
    pub regex_fallback_error: Option<String>,
    pub in_flight: bool,
    pub error: Option<String>,
}

pub fn content_search(snapshot: &Snapshot) -> ContentSearchView {
    let state = &snapshot.sources.content_search;
    let (matches, truncated, regex_fallback_error) = state
        .result
        .as_ref()
        .filter(|(query, _)| state.wanted.as_ref() == Some(query))
        .map(|(_, result): &(ContentSearchQuery, ContentSearch)| {
            (
                result.matches.clone(),
                result.truncated,
                result.regex_fallback_error.clone(),
            )
        })
        .unwrap_or_default();
    ContentSearchView {
        query: state.wanted.clone(),
        matches,
        truncated,
        regex_fallback_error,
        in_flight: state.in_flight,
        error: state.error.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_snapshot_has_no_content_matches() {
        let view = content_search(&Snapshot::default());
        assert!(view.matches.is_empty());
        assert!(!view.in_flight);
    }
}
