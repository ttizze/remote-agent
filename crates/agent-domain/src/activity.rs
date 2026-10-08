//! Display bounds shared by operating-system activity delivery and rendering.

pub const ACTIVITY_SUMMARY_LIMIT: usize = 120;
pub const ACTIVITY_STATUS_LIMIT: usize = 40;
pub const ACTIVITY_LINK_LIMIT: usize = 512;
pub const ACTIVITY_ROWS_LIMIT: usize = 5;
pub const ACTIVITY_MESSAGE_MAX_AGE_MS: i64 = 10 * 60 * 1_000;
pub const RUNNING_ACTIVITY_TTL_MS: i64 = 2 * 60 * 60 * 1_000;
pub const WAITING_ACTIVITY_TTL_MS: i64 = 24 * 60 * 60 * 1_000;
pub const TERMINAL_ACTIVITY_TTL_MS: i64 = 15 * 60 * 1_000;
pub const TERMINAL_NOTIFICATION_FRESHNESS_MS: i64 = 2 * 60 * 1_000;
pub const ACTIVITY_MAX_DISPLAY_LIFETIME_MS: i64 = 24 * 60 * 60 * 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityDeliveryDecision {
    Accept,
    Rearmed,
    IgnoreStale,
    Expired,
    Dismissed,
}

impl ActivityDeliveryDecision {
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Rearmed => "rearmed",
            Self::IgnoreStale => "ignore_stale",
            Self::Expired => "expired",
            Self::Dismissed => "dismissed",
        }
    }
}

/// Returns true only when a provider message is close enough to the local
/// clock in either direction. Native clients must not invent a timestamp
/// window when an FCM/APNs envelope contains the source timestamp.
pub fn activity_message_is_fresh(updated_at_ms: i64, now_ms: i64) -> bool {
    let delta = updated_at_ms
        .checked_sub(now_ms)
        .or_else(|| now_ms.checked_sub(updated_at_ms))
        .unwrap_or(i64::MAX);
    delta <= ACTIVITY_MESSAGE_MAX_AGE_MS && delta >= -ACTIVITY_MESSAGE_MAX_AGE_MS
}

pub fn activity_expiry_at_ms(phase: &str, updated_at_ms: i64) -> i64 {
    let ttl = match canonical_activity_phase(phase) {
        "starting" | "running" => RUNNING_ACTIVITY_TTL_MS,
        "waiting_for_approval" | "waiting_for_input" => WAITING_ACTIVITY_TTL_MS,
        "completed" | "failed" | "stale" => TERMINAL_ACTIVITY_TTL_MS,
        _ => TERMINAL_ACTIVITY_TTL_MS,
    };
    updated_at_ms.saturating_add(ttl)
}

pub fn activity_expiry_is_due(expires_at_ms: i64, now_ms: i64) -> bool {
    expires_at_ms <= now_ms
}

/// Bounds one OS notification timeout while retaining the Host's absolute
/// source expiry. The sentinel is used only by pure unit fixtures and never
/// comes from a Host envelope.
pub fn activity_display_expiry_at_ms(expires_at_ms: i64, now_ms: i64) -> i64 {
    if expires_at_ms == i64::MAX {
        i64::MAX
    } else {
        expires_at_ms.min(now_ms.saturating_add(ACTIVITY_MAX_DISPLAY_LIFETIME_MS))
    }
}

pub fn activity_notification_is_fresh(phase: &str, updated_at_ms: i64, now_ms: i64) -> bool {
    !matches!(
        canonical_activity_phase(phase),
        "completed" | "failed" | "stale"
    ) || {
        let delta = updated_at_ms
            .checked_sub(now_ms)
            .or_else(|| now_ms.checked_sub(updated_at_ms))
            .unwrap_or(i64::MAX);
        delta <= TERMINAL_NOTIFICATION_FRESHNESS_MS && delta >= -TERMINAL_NOTIFICATION_FRESHNESS_MS
    }
}

pub fn activity_delivery_decision(
    updated_at_ms: i64,
    expiry_at_ms: i64,
    now_ms: i64,
    previous_updated_at_ms: i64,
    dismissed: bool,
    active: bool,
    previous_active: bool,
) -> ActivityDeliveryDecision {
    if !activity_message_is_fresh(updated_at_ms, now_ms)
        || (previous_updated_at_ms >= 0 && updated_at_ms < previous_updated_at_ms)
    {
        return ActivityDeliveryDecision::IgnoreStale;
    }
    if expiry_at_ms <= now_ms {
        return ActivityDeliveryDecision::Expired;
    }
    if dismissed {
        return if active && !previous_active {
            ActivityDeliveryDecision::Rearmed
        } else {
            ActivityDeliveryDecision::Dismissed
        };
    }
    ActivityDeliveryDecision::Accept
}

/// A provider-neutral awareness row. The Host and native clients use this
/// input record, while this module owns phase names, ordering, bounds, and
/// the shared ActivityKit/ongoing-notification content state.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityRecord {
    pub environment_id: String,
    pub thread_id: String,
    pub project_title: String,
    pub thread_title: String,
    pub model_title: String,
    pub phase: String,
    pub headline: String,
    pub updated_at_ms: i64,
    pub deep_link: String,
}

/// The exact Codable/JSON record consumed by the iOS ActivityKit extension
/// and Android's core-owned notification projection.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityContentItem {
    pub environment_id: String,
    pub thread_id: String,
    pub project_title: String,
    pub thread_title: String,
    pub model_title: String,
    pub phase: String,
    pub status: String,
    pub updated_at: String,
    pub deep_link: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityContentState {
    pub title: String,
    pub subtitle: String,
    pub active_count: u32,
    pub updated_at: String,
    pub activities: Vec<ActivityContentItem>,
}

/// Projects one Host's immutable awareness rows into the shared native
/// ContentState. Callers supply source facts; they do not choose display
/// order, phase aliases, status text, or row limits.
pub fn activity_content_state(records: &[ActivityRecord]) -> ActivityContentState {
    project_records(records, None, None)
}

/// Aggregates already projected Host states for Android's single ongoing
/// notification. The active count is retained from each Host even when its
/// visible rows were capped before aggregation.
pub fn aggregate_activity_content_states(
    states: &[ActivityContentState],
) -> ActivityContentState {
    let mut records = Vec::new();
    let mut active_count = 0u32;
    let mut latest = 0i64;
    for state in states {
        active_count = active_count.saturating_add(state.active_count);
        latest = latest.max(parse_activity_timestamp(&state.updated_at));
        records.extend(state.activities.iter().enumerate().map(|(index, item)| {
            ActivityRecord {
                environment_id: item.environment_id.clone(),
                thread_id: item.thread_id.clone(),
                project_title: item.project_title.clone(),
                thread_title: item.thread_title.clone(),
                model_title: item.model_title.clone(),
                phase: item.phase.clone(),
                headline: if index == 0 {
                    state.subtitle.clone()
                } else {
                    item.status.clone()
                },
                updated_at_ms: parse_activity_timestamp(&item.updated_at),
                deep_link: item.deep_link.clone(),
            }
        }));
    }
    project_records(&records, Some(active_count), Some(latest))
}

/// JSON entry point used by generated Swift/Kotlin bindings.
pub fn activity_content_state_json(json: &str) -> String {
    let records = serde_json::from_str::<Vec<ActivityRecord>>(json).unwrap_or_default();
    serde_json::to_string(&activity_content_state(&records)).expect("activity content serializes")
}

/// JSON entry point used when Android combines retained per-Host states.
pub fn aggregate_activity_content_states_json(json: &str) -> String {
    let states = serde_json::from_str::<Vec<ActivityContentState>>(json).unwrap_or_default();
    serde_json::to_string(&aggregate_activity_content_states(&states))
        .expect("activity content serializes")
}

fn project_records(
    records: &[ActivityRecord],
    active_count_override: Option<u32>,
    updated_at_override: Option<i64>,
) -> ActivityContentState {
    let mut records = records
        .iter()
        .filter(|record| !record.environment_id.is_empty() && !record.thread_id.is_empty())
        .cloned()
        .map(|mut record| {
            record.phase = canonical_activity_phase(&record.phase).into();
            record
        })
        .collect::<Vec<_>>();
    records.sort_by(|left, right| {
        activity_priority(&left.phase)
            .cmp(&activity_priority(&right.phase))
            .then_with(|| right.updated_at_ms.cmp(&left.updated_at_ms))
            .then_with(|| left.environment_id.cmp(&right.environment_id))
            .then_with(|| left.thread_id.cmp(&right.thread_id))
    });
    let active_count = active_count_override.unwrap_or_else(|| {
        records
            .iter()
            .filter(|record| !is_terminal_phase(&record.phase))
            .count()
            .try_into()
            .unwrap_or(u32::MAX)
    });
    let displayed_active_count = records
        .iter()
        .take(ACTIVITY_ROWS_LIMIT)
        .filter(|record| !is_terminal_phase(&record.phase))
        .count();
    let updated_at = updated_at_override.unwrap_or_else(|| {
        records
            .iter()
            .map(|record| record.updated_at_ms)
            .max()
            .unwrap_or(0)
    });
    let activities = records
        .iter()
        .take(ACTIVITY_ROWS_LIMIT)
        .map(|record| ActivityContentItem {
            environment_id: record.environment_id.clone(),
            thread_id: record.thread_id.clone(),
            project_title: bounded_activity_text(&record.project_title, ACTIVITY_SUMMARY_LIMIT),
            thread_title: bounded_activity_text(&record.thread_title, ACTIVITY_SUMMARY_LIMIT),
            model_title: bounded_activity_text(
                if record.model_title.is_empty() {
                    "Model"
                } else {
                    &record.model_title
                },
                ACTIVITY_SUMMARY_LIMIT,
            ),
            phase: record.phase.clone(),
            status: bounded_activity_text(
                activity_status(&record.phase),
                ACTIVITY_STATUS_LIMIT,
            ),
            updated_at: activity_timestamp(record.updated_at_ms),
            deep_link: bounded_activity_link(&record.deep_link),
        })
        .collect::<Vec<_>>();
    let first = records.first();
    let headline = first
        .map(|record| bounded_activity_text(&record.headline, ACTIVITY_SUMMARY_LIMIT))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Agent activity".into());
    let subtitle = if displayed_active_count <= 1 && activities.len() == 1 {
        headline
    } else if active_count > 0 {
        format!("{active_count} active agent activities")
    } else {
        headline
    };
    ActivityContentState {
        title: first
            .map(|record| bounded_activity_text(&record.project_title, ACTIVITY_SUMMARY_LIMIT))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "Agent activity".into()),
        subtitle,
        active_count,
        updated_at: activity_timestamp(updated_at),
        activities,
    }
}

fn canonical_activity_phase(value: &str) -> &'static str {
    match value {
        "waitingApproval" | "waiting_for_approval" => "waiting_for_approval",
        "waitingInput" | "waiting_for_input" => "waiting_for_input",
        "starting" => "starting",
        "running" => "running",
        "completed" => "completed",
        "failed" => "failed",
        "stale" => "stale",
        _ => "stale",
    }
}

fn is_terminal_phase(phase: &str) -> bool {
    matches!(phase, "completed" | "failed" | "stale")
}

fn activity_priority(phase: &str) -> u8 {
    match phase {
        "waiting_for_approval" | "waiting_for_input" => 0,
        "failed" => 1,
        "starting" | "running" => 2,
        _ => 3,
    }
}

/// Returns the canonical short status shared by push notifications and
/// native activity surfaces. Callers pass the wire phase so they cannot
/// silently invent a second presentation mapping.
pub fn activity_status(phase: &str) -> &'static str {
    match phase {
        "starting" => "Connecting",
        "running" => "Working",
        "waiting_for_approval" => "Approval",
        "waiting_for_input" => "Input",
        "completed" => "Done",
        "failed" => "Failed",
        _ => "Waiting",
    }
}

fn activity_timestamp(millis: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis)
        .unwrap_or_else(|| chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).expect("epoch"))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn activity_timestamp_millis(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|date| date.timestamp_millis())
}

fn parse_activity_timestamp(value: &str) -> i64 {
    activity_timestamp_millis(value).unwrap_or(0)
}

/// Keeps routing identities intact and drops links that cannot safely route
/// back into the app.
pub fn bounded_activity_link(value: &str) -> String {
    if value.encode_utf16().count() > ACTIVITY_LINK_LIMIT {
        return String::new();
    }
    let Ok(url) = url::Url::parse(value) else {
        return String::new();
    };
    let valid = url.scheme() == "remoteagent"
        && url.host_str() == Some("threads")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url
            .path_segments()
            .is_some_and(|segments| {
                let segments = segments.collect::<Vec<_>>();
                segments.len() == 2 && segments.iter().all(|segment| !segment.is_empty())
            });
    valid.then(|| value.to_owned()).unwrap_or_default()
}

/// Trims a display string and bounds it in UTF-16 units without splitting a
/// surrogate pair. The ellipsis is part of the display budget.
pub fn bounded_activity_text(text: &str, max_units: usize) -> String {
    let text = text.trim_matches(|c: char| c == '\u{feff}' || (c != '\u{85}' && c.is_whitespace()));
    if text.encode_utf16().count() <= max_units {
        return text.to_owned();
    }
    let prefix_budget = max_units.saturating_sub(3);
    let mut units = 0;
    let end = text
        .char_indices()
        .find_map(|(offset, c)| {
            units += c.len_utf16();
            (units > prefix_budget).then_some(offset)
        })
        .unwrap_or(text.len());
    let prefix = text[..end]
        .trim_end_matches(|c: char| c == '\u{feff}' || (c != '\u{85}' && c.is_whitespace()));
    format!("{prefix}{}", ".".repeat(max_units.min(3)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn record(thread_id: &str, phase: &str, updated_at_ms: i64) -> ActivityRecord {
        ActivityRecord {
            environment_id: "host".into(),
            thread_id: thread_id.into(),
            project_title: "Project".into(),
            thread_title: thread_id.into(),
            model_title: "Model".into(),
            phase: phase.into(),
            headline: format!("{phase} headline"),
            updated_at_ms,
            deep_link: format!("remoteagent://threads/host/{thread_id}"),
        }
    }

    #[test]
    fn content_state_owns_phase_status_order_and_bounds() {
        let state = activity_content_state(&[
            record("work", "running", 2),
            record("input", "waitingInput", 1),
        ]);
        assert_eq!(state.active_count, 2);
        assert_eq!(state.activities[0].thread_id, "input");
        assert_eq!(state.activities[0].phase, "waiting_for_input");
        assert_eq!(state.activities[0].status, "Input");
        assert_eq!(state.subtitle, "2 active agent activities");
    }

    #[test]
    fn aggregate_preserves_capped_active_counts_and_routes() {
        let mut first = activity_content_state(&[record("one", "running", 1)]);
        first.active_count = 4;
        let second = activity_content_state(&[record("two", "failed", 2)]);
        let aggregate = aggregate_activity_content_states(&[first, second]);
        assert_eq!(aggregate.active_count, 4);
        assert_eq!(aggregate.activities.len(), 2);
        assert_eq!(aggregate.activities[0].thread_id, "one");
        assert_eq!(aggregate.activities[0].deep_link, "remoteagent://threads/host/one");
    }

    #[test]
    fn message_freshness_accepts_small_clock_skew_but_rejects_future_replays() {
        assert!(activity_message_is_fresh(1_000, 1_000 + ACTIVITY_MESSAGE_MAX_AGE_MS));
        assert!(activity_message_is_fresh(1_000 + ACTIVITY_MESSAGE_MAX_AGE_MS, 1_000));
        assert!(!activity_message_is_fresh(
            1_000 + ACTIVITY_MESSAGE_MAX_AGE_MS + 1,
            1_000
        ));
        assert!(!activity_message_is_fresh(
            1_000,
            1_000 + ACTIVITY_MESSAGE_MAX_AGE_MS + 1
        ));
    }

    #[test]
    fn delivery_policy_expires_terminal_cards_and_rearms_new_runs() {
        let now = 10_000;
        let expiry = now + TERMINAL_ACTIVITY_TTL_MS;
        assert_eq!(
            activity_delivery_decision(9_500, expiry, now, -1, false, false, true),
            ActivityDeliveryDecision::Accept
        );
        assert_eq!(
            activity_delivery_decision(9_500, now, now, -1, false, true, false),
            ActivityDeliveryDecision::Expired
        );
        assert_eq!(
            activity_delivery_decision(9_500, expiry, now, 9_000, true, true, false),
            ActivityDeliveryDecision::Rearmed
        );
        assert_eq!(
            activity_delivery_decision(9_500, expiry, now, 9_000, true, false, true),
            ActivityDeliveryDecision::Dismissed
        );
    }

    #[test]
    fn display_expiry_bounds_only_the_os_timeout() {
        let now = 10_000;
        assert_eq!(activity_display_expiry_at_ms(i64::MAX, now), i64::MAX);
        assert_eq!(
            activity_display_expiry_at_ms(
                now + ACTIVITY_MAX_DISPLAY_LIFETIME_MS + 1,
                now,
            ),
            now + ACTIVITY_MAX_DISPLAY_LIFETIME_MS
        );
    }

    #[test]
    fn single_host_aggregate_preserves_its_attention_headline() {
        let state = activity_content_state(&[record("input", "waitingInput", 1)]);
        assert_eq!(aggregate_activity_content_states(&[state]).subtitle, "waitingInput headline");
    }

    #[test]
    fn unsafe_or_oversized_links_are_omitted_without_changing_ids() {
        let mut value = record("thread", "running", 1);
        value.deep_link = "https://example.test/".into();
        let state = activity_content_state(&[value]);
        assert_eq!(state.activities[0].thread_id, "thread");
        assert!(state.activities[0].deep_link.is_empty());
    }

    #[test]
    fn display_bounds_trim_whitespace_and_reserve_the_ellipsis() {
        assert_eq!(
            bounded_activity_text("\u{feff} Project \u{3000}", 120),
            "Project"
        );
        assert_eq!(bounded_activity_text("\u{85}Project", 120), "\u{85}Project");
        assert_eq!(
            bounded_activity_text(&"x".repeat(121), 120),
            format!("{}...", "x".repeat(117))
        );
        assert_eq!(bounded_activity_text("abc  defg", 8), "abc...");
        assert_eq!(bounded_activity_text("😀😀😀", 5), "😀...");
    }

    proptest! {
        #[test]
        fn arbitrary_unicode_never_exceeds_the_display_budget(
            text in prop::collection::vec(any::<char>(), 0..2048),
            budget in 0usize..256,
        ) {
            let text = text.into_iter().collect::<String>();
            let bounded = bounded_activity_text(&text, budget);
            prop_assert!(bounded.encode_utf16().count() <= budget);
            prop_assert!(!bounded.contains('\u{fffd}') || text.contains('\u{fffd}'));
        }
    }
}
