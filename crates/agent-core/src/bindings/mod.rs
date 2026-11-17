//! Native ABI adapters; client workflows live in client modules.
mod json;
mod protocol;
mod snapshot;

use crate::client::{AgentError, error};

/// UTF-8 byte offsets returned by the shared conversation text matcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ConversationTextMatch {
    pub start: u64,
    pub end: u64,
}

#[uniffi::export]
pub fn conversation_text_matches(text: String, query: String) -> Vec<ConversationTextMatch> {
    crate::presentation::conversation::find_matches(&text, &query)
        .into_iter()
        .map(|found| ConversationTextMatch {
            start: found.start as u64,
            end: found.end as u64,
        })
        .collect()
}

#[uniffi::export]
pub fn conversation_search_step(current: u64, count: u64, forward: bool) -> Option<u64> {
    crate::presentation::conversation::adjacent_match_index(current, count, forward)
}

#[uniffi::export]
pub fn browser_address_url(input: String) -> Result<String, AgentError> {
    crate::browser_view::address_url(&input).map_err(error)
}

#[uniffi::export]
pub fn browser_fit_action(
    width: f64,
    height: f64,
    current_width: u32,
    current_height: u32,
) -> Option<crate::browser::BrowserAction> {
    crate::browser_view::fit_action(width, height, current_width, current_height)
}

#[uniffi::export]
pub fn browser_input_focus(
    action: crate::browser::BrowserAction,
    input_kind: Option<crate::browser::BrowserInputKind>,
) -> Option<bool> {
    crate::browser_view::input_focus(&action, input_kind)
}
