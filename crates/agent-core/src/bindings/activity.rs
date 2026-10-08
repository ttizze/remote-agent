//! Provider-neutral awareness projection exposed to native clients.
use agent_domain::{
    activity_content_state_json, activity_delivery_decision, activity_display_expiry_at_ms,
    activity_expiry_is_due, activity_message_is_fresh, activity_timestamp_millis,
    aggregate_activity_content_states_json,
};

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

/// Checks provider-message age using the source timestamp and local clock.
#[uniffi::export]
pub fn agent_activity_message_is_fresh(updated_at_ms: i64, now_ms: i64) -> bool {
    activity_message_is_fresh(updated_at_ms, now_ms)
}

/// Parses an RFC3339 source timestamp without giving native clients their own
/// freshness policy. `-1` means malformed or absent.
#[uniffi::export]
pub fn agent_activity_timestamp_millis(value: String) -> i64 {
    activity_timestamp_millis(&value).unwrap_or(-1)
}

#[uniffi::export]
pub fn agent_activity_expiry_is_due(expires_at_ms: i64, now_ms: i64) -> bool {
    activity_expiry_is_due(expires_at_ms, now_ms)
}

#[uniffi::export]
pub fn agent_activity_display_expiry_at(expires_at_ms: i64, now_ms: i64) -> i64 {
    activity_display_expiry_at_ms(expires_at_ms, now_ms)
}

/// Resolves ordering, absolute expiry and dismiss/re-arm behavior in the
/// shared domain. Native code only persists the returned decision.
#[uniffi::export]
pub fn agent_activity_delivery_decision(
    updated_at_ms: i64,
    expiry_at_ms: i64,
    now_ms: i64,
    previous_updated_at_ms: i64,
    dismissed: bool,
    active: bool,
    previous_active: bool,
) -> String {
    activity_delivery_decision(
        updated_at_ms,
        expiry_at_ms,
        now_ms,
        previous_updated_at_ms,
        dismissed,
        active,
        previous_active,
    )
    .wire_name()
    .into()
}
