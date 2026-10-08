//! Provider-neutral awareness projection exposed to native clients.
use agent_domain::{aggregate_activity_content_states_json, activity_content_state_json};

/// Projects source awareness records into the exact ActivityKit/ongoing
/// notification ContentState JSON shape. The domain crate owns ordering,
/// phase aliases, bounds, and routing link validation.
#[uniffi::export]
pub fn agent_activity_content_state_json(json: String) -> String {
    activity_content_state_json(&json)
}

/// Aggregates retained per-Host ContentState records for Android's one
/// ongoing notification while retaining the domain active count and row
/// selection policy.
#[uniffi::export]
pub fn aggregate_agent_activity_content_states_json(json: String) -> String {
    aggregate_activity_content_states_json(&json)
}
