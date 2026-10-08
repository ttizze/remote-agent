//! Display bounds shared by operating-system activity delivery and rendering.

use percent_encoding::percent_decode_str;

pub const ACTIVITY_SUMMARY_LIMIT: usize = 120;
pub const ACTIVITY_STATUS_LIMIT: usize = 40;
pub const ACTIVITY_LINK_LIMIT: usize = 512;
pub const ACTIVITY_ROWS_LIMIT: usize = 5;
/// Host-independent route used when one alert represents several rows.
pub const ACTIVITY_OVERVIEW_DEEP_LINK: &str = "remoteagent://overview";
const ACTIVITY_THREAD_DEEP_LINK_PREFIX: &str = "remoteagent://threads/";

/// A validated activity deep link. The route keeps environment and thread
/// identity together so native clients do not open a same-named thread on a
/// different Host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivityDeepLink {
    Overview,
    Thread {
        environment_id: String,
        thread_id: String,
    },
}

/// Builds the canonical environment-qualified activity route used by
/// notification decisions and native click handlers.
pub fn activity_thread_deep_link(environment_id: &str, thread_id: &str) -> String {
    format!(
        "remoteagent://threads/{}/{}",
        encode_route_segment(environment_id),
        encode_route_segment(thread_id),
    )
}

fn encode_route_segment(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                vec![char::from(byte)]
            }
            byte => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}

/// Parses the canonical routes shared by desktop and native click consumers.
/// This intentionally parses the raw route instead of using a URL normalizer:
/// malformed escapes and surrounding whitespace must not become a different
/// target before the identity segments are validated.
pub fn parse_activity_deep_link(value: &str) -> Option<ActivityDeepLink> {
    if value == ACTIVITY_OVERVIEW_DEEP_LINK {
        return Some(ActivityDeepLink::Overview);
    }

    if value.trim_matches(|character: char| {
        character == '\u{feff}' || (character != '\u{85}' && character.is_whitespace())
    }) != value
        || value.contains('?')
        || value.contains('#')
    {
        return None;
    }

    let path = value.strip_prefix(ACTIVITY_THREAD_DEEP_LINK_PREFIX)?;
    let mut segments = path.split('/');
    let environment_id = decode_route_segment(segments.next()?)?;
    let thread_id = decode_route_segment(segments.next()?)?;
    segments
        .next()
        .is_none()
        .then_some(ActivityDeepLink::Thread {
            environment_id,
            thread_id,
        })
}

/// Chooses and canonicalizes the route carried by a notification. An exact
/// Overview route wins immediately; a valid thread route is decoded and
/// rebuilt so equivalent percent encodings share one native target. Invalid
/// explicit routes fall back to the non-empty environment and thread ids.
pub fn activity_notification_deep_link(
    value: Option<&str>,
    environment_id: Option<&str>,
    thread_id: Option<&str>,
) -> Option<String> {
    if let Some(value) = value {
        match parse_activity_deep_link(value) {
            Some(ActivityDeepLink::Overview) => return Some(ACTIVITY_OVERVIEW_DEEP_LINK.into()),
            Some(ActivityDeepLink::Thread {
                environment_id,
                thread_id,
            }) => return Some(activity_thread_deep_link(&environment_id, &thread_id)),
            None => {}
        }
    }

    match (environment_id, thread_id) {
        (Some(environment_id), Some(thread_id))
            if !environment_id.is_empty() && !thread_id.is_empty() =>
        {
            Some(activity_thread_deep_link(environment_id, thread_id))
        }
        _ => None,
    }
}

fn decode_route_segment(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = *bytes.get(index + 1)?;
            let low = *bytes.get(index + 2)?;
            if !high.is_ascii_hexdigit() || !low.is_ascii_hexdigit() {
                return None;
            }
            index += 3;
        } else {
            index += 1;
        }
    }

    (!segment.is_empty())
        .then(|| percent_decode_str(segment).decode_utf8().ok())
        .flatten()
        .filter(|segment| {
            let value = segment.as_ref();
            !value.is_empty()
                && value != "."
                && value != ".."
                && !value
                    .chars()
                    .any(|character| character == '\\' || character.is_control())
        })
        .map(|segment| segment.into_owned())
}
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
    (-ACTIVITY_MESSAGE_MAX_AGE_MS..=ACTIVITY_MESSAGE_MAX_AGE_MS).contains(&delta)
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
    expires_at_ms < now_ms
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
    ) || { now_ms.saturating_sub(updated_at_ms) <= TERMINAL_NOTIFICATION_FRESHNESS_MS }
}

/// Each input is an independent provider or persisted-state fact. Keep them
/// explicit so callers cannot hide stale, expiry, or dismissal state in a
/// mutable wrapper.
#[allow(clippy::too_many_arguments)]
pub fn activity_delivery_decision(
    delivery_updated_at_ms: i64,
    source_updated_at_ms: i64,
    expiry_at_ms: i64,
    now_ms: i64,
    previous_source_updated_at_ms: i64,
    dismissed: bool,
    active: bool,
    previous_active: bool,
) -> ActivityDeliveryDecision {
    if !activity_message_is_fresh(delivery_updated_at_ms, now_ms)
        || (previous_source_updated_at_ms >= 0
            && source_updated_at_ms < previous_source_updated_at_ms)
    {
        return ActivityDeliveryDecision::IgnoreStale;
    }
    if expiry_at_ms < now_ms {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityAlert {
    pub title: String,
    pub body: String,
    pub identity: String,
    pub deep_link: String,
}

/// Resolves only newly entered attention/terminal rows. Presentation updates
/// to an already waiting or completed row stay silent; the caller may still
/// deliver the updated activity content state.
///
/// The notification gates remain explicit because they come from separate
/// persisted preferences and platform authorization facts.
#[allow(clippy::too_many_arguments)]
pub fn activity_alert_for_transition(
    previous: &[ActivityRecord],
    next: &[ActivityRecord],
    previous_available: bool,
    now_ms: i64,
    notifications_enabled: bool,
    notify_on_approval: bool,
    notify_on_input: bool,
    notify_on_completion: bool,
    notify_on_failure: bool,
) -> Option<ActivityAlert> {
    if !notifications_enabled || !previous_available {
        return None;
    }
    let previous_attention = previous
        .iter()
        .filter(|record| is_attention_phase(&record.phase))
        .map(activity_row_key)
        .collect::<std::collections::HashSet<_>>();
    let previous_phases = previous
        .iter()
        .map(|record| {
            (
                activity_row_key(record),
                canonical_activity_phase(&record.phase),
            )
        })
        .collect::<std::collections::HashMap<_, _>>();
    let mut ordered = next
        .iter()
        .filter(|record| !record.environment_id.is_empty() && !record.thread_id.is_empty())
        .cloned()
        .collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        activity_priority(canonical_activity_phase(&left.phase))
            .cmp(&activity_priority(canonical_activity_phase(&right.phase)))
            .then_with(|| right.updated_at_ms.cmp(&left.updated_at_ms))
            .then_with(|| left.environment_id.cmp(&right.environment_id))
            .then_with(|| left.thread_id.cmp(&right.thread_id))
    });
    ordered.truncate(ACTIVITY_ROWS_LIMIT);

    let attention = ordered
        .iter()
        .filter(|record| {
            is_attention_phase(&record.phase)
                && !previous_attention.contains(&activity_row_key(record))
                && alert_allowed_for_phase(
                    &record.phase,
                    notify_on_approval,
                    notify_on_input,
                    notify_on_completion,
                    notify_on_failure,
                )
        })
        .cloned()
        .collect::<Vec<_>>();
    let rows = if attention.is_empty() {
        ordered
            .iter()
            .filter(|record| is_alert_terminal_phase(&record.phase))
            .filter(|record| {
                let key = activity_row_key(record);
                let prior = previous_phases.get(&key).copied();
                (prior.is_some_and(|phase| !is_alert_terminal_phase(phase))
                    || (prior.is_none() && previous_available))
                    && alert_allowed_for_phase(
                        &record.phase,
                        notify_on_approval,
                        notify_on_input,
                        notify_on_completion,
                        notify_on_failure,
                    )
                    && activity_notification_is_fresh(&record.phase, record.updated_at_ms, now_ms)
            })
            .cloned()
            .collect::<Vec<_>>()
    } else {
        attention
    };
    let first = rows.first()?;
    let is_attention = is_attention_phase(&first.phase);
    let (title, body) = if rows.len() == 1 {
        (
            bounded_activity_text(&first.thread_title, ACTIVITY_SUMMARY_LIMIT),
            bounded_activity_text(
                &format!("{}: {}", activity_status(&first.phase), first.project_title),
                ACTIVITY_SUMMARY_LIMIT,
            ),
        )
    } else {
        (
            format!(
                "{} agents {}",
                rows.len(),
                if is_attention {
                    "need attention"
                } else {
                    "finished"
                }
            ),
            bounded_activity_text(
                &rows
                    .iter()
                    .map(|row| row.thread_title.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                ACTIVITY_SUMMARY_LIMIT * 5,
            ),
        )
    };
    let identity = activity_alert_identity(&rows);
    Some(ActivityAlert {
        title,
        body,
        identity,
        deep_link: if rows.len() == 1 {
            first.deep_link.clone()
        } else {
            ACTIVITY_OVERVIEW_DEEP_LINK.into()
        },
    })
}

fn activity_alert_identity(rows: &[ActivityRecord]) -> String {
    let mut tuples = rows
        .iter()
        .map(|row| {
            [
                row.environment_id.clone(),
                row.thread_id.clone(),
                canonical_activity_phase(&row.phase).into(),
                activity_timestamp(row.updated_at_ms),
            ]
        })
        .collect::<Vec<[String; 4]>>();
    tuples.sort();
    if tuples.len() == 1 {
        serde_json::to_string(&tuples[0]).expect("activity alert identity serializes")
    } else {
        serde_json::to_string(&tuples).expect("activity alert identities serialize")
    }
}

fn activity_row_key(record: &ActivityRecord) -> (String, String) {
    (record.environment_id.clone(), record.thread_id.clone())
}

fn is_attention_phase(phase: &str) -> bool {
    matches!(
        canonical_activity_phase(phase),
        "waiting_for_approval" | "waiting_for_input"
    )
}

fn is_alert_terminal_phase(phase: &str) -> bool {
    matches!(canonical_activity_phase(phase), "completed" | "failed")
}

fn alert_allowed_for_phase(
    phase: &str,
    notify_on_approval: bool,
    notify_on_input: bool,
    notify_on_completion: bool,
    notify_on_failure: bool,
) -> bool {
    match canonical_activity_phase(phase) {
        "waiting_for_approval" => notify_on_approval,
        "waiting_for_input" => notify_on_input,
        "completed" => notify_on_completion,
        "failed" => notify_on_failure,
        _ => false,
    }
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
    project_records(records, None, None, false)
}

/// Aggregates already projected Host states for Android's single ongoing
/// notification. The active count is retained from each Host even when its
/// visible rows were capped before aggregation.
pub fn aggregate_activity_content_states(states: &[ActivityContentState]) -> ActivityContentState {
    let mut records = Vec::new();
    let mut active_count = 0u32;
    let mut latest = 0i64;
    for state in states {
        active_count = active_count.saturating_add(state.active_count);
        latest = latest.max(parse_activity_timestamp(&state.updated_at));
        records.extend(
            state
                .activities
                .iter()
                .enumerate()
                .map(|(index, item)| ActivityRecord {
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
                }),
        );
    }
    project_records(&records, Some(active_count), Some(latest), true)
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
    active_rows_first: bool,
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
        if active_rows_first {
            let left_terminal = is_terminal_phase(&left.phase);
            let right_terminal = is_terminal_phase(&right.phase);
            if left_terminal != right_terminal {
                return left_terminal.cmp(&right_terminal);
            }
            if left_terminal {
                return right
                    .updated_at_ms
                    .cmp(&left.updated_at_ms)
                    .then_with(|| left.environment_id.cmp(&right.environment_id))
                    .then_with(|| left.thread_id.cmp(&right.thread_id));
            }
        }
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
            status: bounded_activity_text(activity_status(&record.phase), ACTIVITY_STATUS_LIMIT),
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
    if parse_activity_deep_link(value).is_some() {
        value.to_owned()
    } else {
        String::new()
    }
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

    fn safe_route_scalar() -> impl Strategy<Value = char> {
        any::<char>().prop_filter("safe activity route scalar", |character| {
            *character != '\\' && !character.is_control()
        })
    }

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
        assert_eq!(
            aggregate.activities[0].deep_link,
            "remoteagent://threads/host/one"
        );
    }

    #[test]
    fn message_freshness_accepts_small_clock_skew_but_rejects_future_replays() {
        assert!(activity_message_is_fresh(
            1_000,
            1_000 + ACTIVITY_MESSAGE_MAX_AGE_MS
        ));
        assert!(activity_message_is_fresh(
            1_000 + ACTIVITY_MESSAGE_MAX_AGE_MS,
            1_000
        ));
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
            activity_delivery_decision(9_500, 9_500, expiry, now, -1, false, false, true),
            ActivityDeliveryDecision::Accept
        );
        assert_eq!(
            activity_delivery_decision(9_500, 9_500, now - 1, now, -1, false, true, false),
            ActivityDeliveryDecision::Expired
        );
        assert_eq!(
            activity_delivery_decision(9_500, 9_500, expiry, now, 9_000, true, true, false),
            ActivityDeliveryDecision::Rearmed
        );
        assert_eq!(
            activity_delivery_decision(9_500, 9_500, expiry, now, 9_000, true, false, true),
            ActivityDeliveryDecision::Dismissed
        );
        assert_eq!(
            activity_delivery_decision(9_500, 9_500, now, now, -1, false, true, false),
            ActivityDeliveryDecision::Accept
        );
    }

    #[test]
    fn display_expiry_bounds_only_the_os_timeout() {
        let now = 10_000;
        assert_eq!(activity_display_expiry_at_ms(i64::MAX, now), i64::MAX);
        assert_eq!(
            activity_display_expiry_at_ms(now + ACTIVITY_MAX_DISPLAY_LIFETIME_MS + 1, now,),
            now + ACTIVITY_MAX_DISPLAY_LIFETIME_MS
        );
    }

    #[test]
    fn alerts_only_new_attention_and_terminal_transitions() {
        let previous = vec![record("thread", "running", 100)];
        let waiting = vec![record("thread", "waiting_for_input", 200)];
        let alert = activity_alert_for_transition(
            &previous, &waiting, true, 200, true, true, true, true, true,
        )
        .expect("new attention should alert");
        assert_eq!(alert.title, "thread");
        assert_eq!(alert.body, "Input: Project");
        assert!(
            activity_alert_for_transition(
                &waiting, &waiting, true, 201, true, true, true, true, true,
            )
            .is_none()
        );
        let mut waiting_refresh = waiting[0].clone();
        waiting_refresh.headline = "Updated title".into();
        waiting_refresh.updated_at_ms = 201;
        assert!(
            activity_alert_for_transition(
                &waiting,
                &[waiting_refresh],
                true,
                201,
                true,
                true,
                true,
                true,
                true,
            )
            .is_none()
        );

        let completed = vec![record("thread", "completed", 300)];
        assert!(
            activity_alert_for_transition(
                &waiting, &completed, true, 300, true, true, true, true, true,
            )
            .is_some()
        );
        let mut renamed = completed[0].clone();
        renamed.headline = "Different title".into();
        assert!(
            activity_alert_for_transition(
                &completed,
                &[renamed],
                true,
                301,
                true,
                true,
                true,
                true,
                true,
            )
            .is_none()
        );
        assert!(activity_notification_is_fresh(
            "completed",
            10_000 + TERMINAL_NOTIFICATION_FRESHNESS_MS + 1,
            10_000,
        ));
    }

    #[test]
    fn alerts_group_new_attention_rows_with_stable_identity() {
        let previous = vec![record("one", "running", 100), record("two", "running", 100)];
        let next = vec![
            record("one", "waiting_for_approval", 200),
            record("two", "waiting_for_input", 201),
        ];
        let alert = activity_alert_for_transition(
            &previous, &next, true, 201, true, true, true, true, true,
        )
        .expect("new attention rows should group");
        assert_eq!(alert.title, "2 agents need attention");
        assert_eq!(alert.deep_link, ACTIVITY_OVERVIEW_DEEP_LINK);
        let identity: Vec<[String; 4]> = serde_json::from_str(&alert.identity).unwrap();
        assert_eq!(identity.len(), 2);
        assert_eq!(identity[0][0], "host");
        assert_eq!(identity[0][3], "1970-01-01T00:00:00.200Z");
        assert_eq!(
            activity_alert_for_transition(&next, &next, true, 202, true, true, true, true, true,),
            None
        );
        assert!(
            activity_alert_for_transition(&[], &next, false, 201, true, true, true, true, true,)
                .is_none()
        );
    }

    #[test]
    fn alert_identity_is_structured_for_new_runs_and_slash_collisions() {
        let previous = vec![record("thread", "running", 100)];
        let first = vec![record("thread", "completed", 200)];
        let second = vec![record("thread", "completed", 300)];
        let first_alert = activity_alert_for_transition(
            &previous, &first, true, 200, true, true, true, true, true,
        )
        .unwrap();
        let second_alert = activity_alert_for_transition(
            &previous, &second, true, 300, true, true, true, true, true,
        )
        .unwrap();
        assert_ne!(first_alert.identity, second_alert.identity);

        let mut left = record("b/c", "running", 100);
        left.environment_id = "a".into();
        let mut right = record("c", "running", 100);
        right.environment_id = "a/b".into();
        let left_alert = activity_alert_for_transition(
            &[left.clone()],
            &[ActivityRecord {
                phase: "waiting_for_input".into(),
                updated_at_ms: 200,
                ..left.clone()
            }],
            true,
            200,
            true,
            true,
            true,
            true,
            true,
        )
        .unwrap();
        let right_alert = activity_alert_for_transition(
            &[right.clone()],
            &[ActivityRecord {
                phase: "waiting_for_input".into(),
                updated_at_ms: 200,
                ..right.clone()
            }],
            true,
            200,
            true,
            true,
            true,
            true,
            true,
        )
        .unwrap();
        assert_ne!(left_alert.identity, right_alert.identity);

        let grouped = activity_alert_for_transition(
            &[left.clone(), right.clone()],
            &[
                ActivityRecord {
                    phase: "waiting_for_input".into(),
                    updated_at_ms: 200,
                    ..left
                },
                ActivityRecord {
                    phase: "waiting_for_approval".into(),
                    updated_at_ms: 200,
                    ..right
                },
            ],
            true,
            200,
            true,
            true,
            true,
            true,
            true,
        )
        .expect("slash-distinct rows should both transition");
        let grouped_identity: Vec<[String; 4]> = serde_json::from_str(&grouped.identity).unwrap();
        assert_eq!(grouped_identity.len(), 2);
    }

    #[test]
    fn single_host_aggregate_preserves_its_attention_headline() {
        let state = activity_content_state(&[record("input", "waitingInput", 1)]);
        assert_eq!(
            aggregate_activity_content_states(&[state]).subtitle,
            "waitingInput headline"
        );
    }

    #[test]
    fn unsafe_or_oversized_links_are_omitted_without_changing_ids() {
        let mut value = record("thread", "running", 1);
        value.deep_link = "https://example.test/".into();
        let state = activity_content_state(&[value]);
        assert_eq!(state.activities[0].thread_id, "thread");
        assert!(state.activities[0].deep_link.is_empty());
        assert_eq!(
            bounded_activity_link(ACTIVITY_OVERVIEW_DEEP_LINK),
            ACTIVITY_OVERVIEW_DEEP_LINK
        );
        assert!(bounded_activity_link("remoteagent://overview/").is_empty());
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

    #[test]
    fn activity_routes_require_a_host_and_thread_or_the_shared_overview() {
        assert_eq!(
            parse_activity_deep_link(ACTIVITY_OVERVIEW_DEEP_LINK),
            Some(ActivityDeepLink::Overview)
        );
        assert!(parse_activity_deep_link("remoteagent://overview/").is_none());
        assert_eq!(
            parse_activity_deep_link("remoteagent://threads/host-a/thread-1"),
            Some(ActivityDeepLink::Thread {
                environment_id: "host-a".into(),
                thread_id: "thread-1".into(),
            })
        );
        assert_eq!(
            parse_activity_deep_link("remoteagent://threads/host%2Fa/thread%2Fb"),
            Some(ActivityDeepLink::Thread {
                environment_id: "host/a".into(),
                thread_id: "thread/b".into(),
            })
        );
        assert!(parse_activity_deep_link("remoteagent://threads/thread-1").is_none());
        assert!(parse_activity_deep_link("remote-agent://thread/host-a:thread-1").is_none());
        assert_eq!(
            activity_thread_deep_link("host/a", "thread/b"),
            "remoteagent://threads/host%2Fa/thread%2Fb"
        );
        assert!(parse_activity_deep_link("remoteagent://threads/host-a/thread-1?open=1").is_none());
    }

    #[test]
    fn activity_routes_reject_malformed_escapes_and_unsafe_decoded_segments() {
        for escape in ["%", "%0", "%gg", "%g0", "%0g", "%ff"] {
            assert!(
                parse_activity_deep_link(&format!("remoteagent://threads/host-a/thread-1{escape}"))
                    .is_none(),
                "malformed escape should be rejected: {escape}"
            );
        }
        for link in [
            "remoteagent://threads/host-a/%5C",
            "remoteagent://threads/host-a/%0A",
            "remoteagent://threads/host-a/%2E",
            "remoteagent://threads/host-a/%2E%2E",
            " remoteagent://threads/host-a/thread-1",
            "remoteagent://threads/host-a/thread-1 ",
            "\u{feff}remoteagent://threads/host-a/thread-1",
            "remoteagent://threads/host-a/thread-1\u{feff}",
            "\u{85}remoteagent://threads/host-a/thread-1",
            "remoteagent://threads/host-a/thread-1\u{85}",
            "remoteagent://threads/host-a/thread-1?open=1",
            "remoteagent://threads/host-a/thread-1#details",
        ] {
            assert!(
                parse_activity_deep_link(link).is_none(),
                "unsafe link: {link}"
            );
        }
    }

    #[test]
    fn activity_routes_roundtrip_unicode_without_parser_length_cap() {
        let environment_id = "東京/環境";
        let thread_id = format!("スレッド😀{}", "x".repeat(ACTIVITY_LINK_LIMIT));
        let link = activity_thread_deep_link(environment_id, &thread_id);

        assert!(link.encode_utf16().count() > ACTIVITY_LINK_LIMIT);
        assert_eq!(
            parse_activity_deep_link(&link),
            Some(ActivityDeepLink::Thread {
                environment_id: environment_id.into(),
                thread_id,
            })
        );
        assert!(bounded_activity_link(&link).is_empty());
    }

    #[test]
    fn notification_routes_normalize_explicit_values_before_fallback() {
        assert_eq!(
            activity_notification_deep_link(
                Some("remoteagent://threads/env%20%E6%9D%B1/thread%2f%E3%81%82"),
                None,
                None,
            ),
            Some("remoteagent://threads/env%20%E6%9D%B1/thread%2F%E3%81%82".into())
        );
        assert_eq!(
            activity_notification_deep_link(
                Some("remoteagent://threads/env/%"),
                Some("fallback env"),
                Some("thread/2"),
            ),
            Some("remoteagent://threads/fallback%20env/thread%2F2".into())
        );
        assert_eq!(
            activity_notification_deep_link(
                Some(ACTIVITY_OVERVIEW_DEEP_LINK),
                Some("ignored"),
                Some("ignored"),
            ),
            Some(ACTIVITY_OVERVIEW_DEEP_LINK.into())
        );
    }

    #[test]
    fn notification_routes_require_both_non_empty_fallback_ids() {
        assert_eq!(
            activity_notification_deep_link(None, Some(""), Some("thread")),
            None
        );
        assert_eq!(
            activity_notification_deep_link(None, Some("environment"), Some("")),
            None
        );
        assert_eq!(
            activity_notification_deep_link(Some(""), Some("environment"), Some("thread")),
            Some("remoteagent://threads/environment/thread".into())
        );
        assert_eq!(
            activity_notification_deep_link(Some("invalid"), None, None),
            None
        );
    }

    proptest! {
        #[test]
        fn safe_unicode_activity_routes_roundtrip(
            environment in prop::collection::vec(safe_route_scalar(), 1..32),
            thread in prop::collection::vec(safe_route_scalar(), 1..32),
        ) {
            let environment_id = environment.into_iter().collect::<String>();
            let thread_id = thread.into_iter().collect::<String>();
            prop_assume!(environment_id != "." && environment_id != "..");
            prop_assume!(thread_id != "." && thread_id != "..");

            let link = activity_thread_deep_link(&environment_id, &thread_id);
            prop_assert_eq!(
                parse_activity_deep_link(&link),
                Some(ActivityDeepLink::Thread {
                    environment_id,
                    thread_id,
                })
            );
        }
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
            prop_assert!(
                !bounded.contains('\u{fffd}') || text.contains('\u{fffd}'),
                "bounded text must not introduce U+FFFD"
            );
        }
    }
}
