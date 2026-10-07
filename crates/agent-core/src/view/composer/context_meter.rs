//! The context-window meter beside the send button and the resume-compaction
//! offer.
use agent_domain::{Driver, ItemKind, RequestBody, RequestStatus, State};

const RESUME_COMPACTION_MINUTES: i64 = 70;
const RESUME_COMPACTION_TOKENS: u64 = 100_000;
pub const RESUME_COMPACTION_NEVER_ANSWER: &str = "Don't ask again";

/// The latest context occupancy the thread reported.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ContextWindowSnapshot {
    pub used_tokens: u64,
    pub total_processed_tokens: Option<u64>,
    pub max_tokens: Option<u64>,
    pub remaining_tokens: Option<u64>,
    pub used_percentage: Option<f64>,
    pub remaining_percentage: Option<f64>,
    pub compacts_automatically: bool,
    pub auto_compact_threshold: Option<u64>,
    pub updated_at_ms: i64,
}

fn snapshot(
    used_tokens: u64,
    max_tokens: Option<u64>,
    total_processed_tokens: Option<u64>,
    auto_compact_threshold: Option<u64>,
    updated_at_ms: i64,
) -> ContextWindowSnapshot {
    let used_percentage = max_tokens
        .filter(|max| *max > 0)
        .map(|max| (used_tokens as f64 / max as f64 * 100.0).min(100.0));
    ContextWindowSnapshot {
        used_tokens,
        total_processed_tokens,
        max_tokens,
        remaining_tokens: max_tokens.map(|max| max.saturating_sub(used_tokens)),
        used_percentage,
        remaining_percentage: used_percentage.map(|used| (100.0 - used).max(0.0)),
        compacts_automatically: true,
        auto_compact_threshold,
        updated_at_ms,
    }
}

/// Prefers the newest attempt's reported occupancy, then the native session's,
/// then the size after the latest compaction.
pub fn latest_context_window(state: &State) -> Option<ContextWindowSnapshot> {
    if let Some((attempt, usage)) = state
        .attempts
        .iter()
        .rev()
        .find_map(|attempt| attempt.context_usage.as_ref().map(|usage| (attempt, usage)))
    {
        let at = attempt.completed_at.as_ref().unwrap_or(&attempt.started_at);
        return Some(snapshot(
            usage.used_tokens,
            usage.max_tokens,
            None,
            usage.auto_compact_threshold,
            at.millis(),
        ));
    }
    if let Some(usage) = &state.native_context_usage {
        let at = state
            .thread
            .as_ref()
            .map_or(0, |thread| thread.updated_at.millis());
        return Some(snapshot(
            usage.used_tokens,
            usage.max_tokens,
            None,
            usage.auto_compact_threshold,
            at,
        ));
    }
    state.visible_items().into_iter().rev().find_map(|item| {
        let ItemKind::Compaction {
            before,
            after: Some(after),
        } = &item.kind
        else {
            return None;
        };
        Some(snapshot(
            *after,
            None,
            *before,
            None,
            item.started_at.millis(),
        ))
    })
}

/// `1.2k`, `45k`, `1.5m`.
pub fn format_context_window_tokens(value: Option<u64>) -> String {
    let Some(value) = value else {
        return "0".into();
    };
    // Tenths rounded half up, as the web formats them.
    let tenths = |unit: u64| {
        let tenths = (value + unit / 20) / (unit / 10);
        match tenths % 10 {
            0 => (tenths / 10).to_string(),
            digit => format!("{}.{digit}", tenths / 10),
        }
    };
    match value {
        0..1_000 => value.to_string(),
        1_000..10_000 => format!("{}k", tenths(1_000)),
        10_000..1_000_000 => format!("{}k", (value + 500) / 1_000),
        _ => format!("{}m", tenths(1_000_000)),
    }
}

fn format_percentage(value: Option<f64>) -> Option<String> {
    let value = value.filter(|value| value.is_finite())?;
    Some(if value < 10.0 {
        let text = format!("{value:.1}");
        format!("{}%", text.strip_suffix(".0").unwrap_or(&text))
    } else {
        format!("{}%", value.round())
    })
}

pub fn context_window_compaction_message(
    model_display_name: Option<&str>,
    auto_compact_threshold: Option<u64>,
) -> String {
    if let Some(threshold) = auto_compact_threshold.filter(|threshold| *threshold > 0) {
        return format!(
            "Compacts automatically at {} tokens.",
            super::group_thousands(threshold)
        );
    }
    match model_display_name {
        Some(name) if !name.is_empty() => {
            format!("Context for {name} compacts automatically when needed.")
        }
        _ => "Context compacts automatically when needed.".into(),
    }
}

/// The catalog's short name for the selected model, else its slug.
pub fn context_window_model_display_name(model: &str, catalog_name: Option<&str>) -> String {
    catalog_name.unwrap_or(model).into()
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CompactContextButton {
    pub label: String,
    pub disabled: bool,
    /// Shown under the button while it is disabled.
    pub disabled_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ContextWindowMeter {
    pub accessibility_label: String,
    /// `45%` when the window size is known.
    pub used_percentage_label: Option<String>,
    /// `90k/200k` when the window size is known, else `90k`.
    pub tokens_label: String,
    /// 0-100, for the ring and the bar; `None` hides the bar.
    pub progress: Option<f64>,
    pub ring_progress: f64,
    pub overloaded: bool,
    pub total_processed_label: Option<String>,
    pub compaction_message: Option<String>,
    pub compact: Option<CompactContextButton>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompactControl {
    /// The selected provider offers `/compact`.
    pub available: bool,
    pub disabled: bool,
    pub disabled_reason: Option<String>,
}

pub fn context_window_meter(
    usage: &ContextWindowSnapshot,
    model_display_name: Option<&str>,
    compact: &CompactControl,
) -> ContextWindowMeter {
    let used_percentage_label = format_percentage(usage.used_percentage);
    let normalized = usage.used_percentage.unwrap_or(0.0).clamp(0.0, 100.0);
    let known = usage.max_tokens.is_some();
    let used = format_context_window_tokens(Some(usage.used_tokens));
    ContextWindowMeter {
        accessibility_label: match (&used_percentage_label, known) {
            (Some(percentage), true) => format!("Context window {percentage} used"),
            _ => format!("Context window {used} tokens used"),
        },
        tokens_label: match (&used_percentage_label, known) {
            (Some(_), true) => format!("{used}/{}", format_context_window_tokens(usage.max_tokens)),
            _ => used,
        },
        used_percentage_label: used_percentage_label.filter(|_| known),
        progress: known.then_some(normalized),
        ring_progress: normalized,
        overloaded: normalized > 90.0,
        total_processed_label: usage
            .total_processed_tokens
            .filter(|tokens| *tokens > 0)
            .map(|tokens| format_context_window_tokens(Some(tokens))),
        compaction_message: usage.compacts_automatically.then(|| {
            context_window_compaction_message(model_display_name, usage.auto_compact_threshold)
        }),
        compact: compact.available.then(|| CompactContextButton {
            label: "Compact context".into(),
            disabled: compact.disabled,
            disabled_reason: compact.disabled_reason.clone().filter(|_| compact.disabled),
        }),
    }
}

/// A provider instance as the compaction check sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionProvider {
    pub instance_id: String,
    pub driver: Driver,
    pub continuation_group_key: Option<String>,
    pub enabled: bool,
    pub available: bool,
    pub ready: bool,
    pub errored: bool,
    pub supports_compact: bool,
}

/// Whether `/compact` can run on the provider that would take the next turn:
/// the requested instance when selectable, else a ready or non-error fallback
/// that shares the locked instance's continuation group.
pub fn has_available_compaction_provider(
    providers: &[CompactionProvider],
    driver: Driver,
    instance_id: Option<&str>,
    locked_instance_id: Option<&str>,
) -> bool {
    let same_driver: Vec<_> = providers.iter().filter(|p| p.driver == driver).collect();
    let locked_group = locked_instance_id.and_then(|locked| {
        same_driver
            .iter()
            .find(|p| p.instance_id == locked)
            .and_then(|p| p.continuation_group_key.clone())
    });
    let compatible: Vec<_> = same_driver
        .into_iter()
        .filter(|p| {
            locked_group
                .as_ref()
                .is_none_or(|group| p.continuation_group_key.as_ref() == Some(group))
        })
        .collect();
    let selectable = |p: &&CompactionProvider| p.enabled && p.available;
    let requested = instance_id
        .and_then(|id| compatible.iter().find(|p| p.instance_id == id))
        .filter(|p| selectable(p));
    requested
        .or_else(|| compatible.iter().find(|p| selectable(p) && p.ready))
        .or_else(|| compatible.iter().find(|p| selectable(p) && !p.errored))
        .is_some_and(|p| p.supports_compact)
}

/// Mirrors the question the Claude resume dialog asks.
pub fn is_resume_compaction_question(question: &str) -> bool {
    let Some(rest) = question.strip_prefix("This session is ") else {
        return false;
    };
    let digits = |text: &str| -> usize { text.chars().take_while(char::is_ascii_digit).count() };
    let after_age = {
        let hours = digits(rest);
        if hours > 0 && rest[hours..].starts_with("h ") {
            let minutes_at = hours + 2;
            let minutes = digits(&rest[minutes_at..]);
            (minutes > 0 && rest[minutes_at + minutes..].starts_with('m'))
                .then(|| &rest[minutes_at + minutes + 1..])
        } else {
            (hours > 0 && rest[hours..].starts_with('m')).then(|| &rest[hours + 1..])
        }
    };
    let Some(rest) = after_age.and_then(|rest| rest.strip_prefix(" old and uses ")) else {
        return false;
    };
    let Some(count) = rest.strip_suffix(" tokens. Compact it before continuing?") else {
        return false;
    };
    let mut groups = count.split(',');
    let first = groups.next().unwrap_or_default();
    (1..=3).contains(&first.len())
        && first.chars().all(|c| c.is_ascii_digit())
        && groups.all(|group| group.len() == 3 && group.chars().all(|c| c.is_ascii_digit()))
}

/// The user told the resume dialog never to ask again.
pub fn has_dismissed_resume_compaction(state: &State) -> bool {
    state.requests.iter().any(|request| {
        request.status == RequestStatus::Resolved
            && matches!(request.body, RequestBody::Questions { .. })
            && request.answers.as_ref().is_some_and(|answers| {
                answers.iter().any(|(question, answer)| {
                    is_resume_compaction_question(question)
                        && answer.text() == RESUME_COMPACTION_NEVER_ANSWER
                })
            })
    })
}

/// Old, large Claude sessions are offered compaction before resuming.
pub fn should_offer_resume_compaction(
    driver: Option<Driver>,
    used_tokens: Option<u64>,
    updated_at_ms: Option<i64>,
    now_ms: i64,
) -> bool {
    driver == Some(Driver::Claude)
        && used_tokens.unwrap_or(0) >= RESUME_COMPACTION_TOKENS
        && updated_at_ms
            .is_some_and(|updated| now_ms - updated >= RESUME_COMPACTION_MINUTES * 60_000)
}

/// "Resume with less context": the banner above the composer offering to
/// compact an old, large Claude session before continuing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeCompactionBanner {
    /// One per thread and context snapshot; "Keep full history" hides it.
    pub key: String,
    pub title: String,
    pub description: String,
    pub compact_label: String,
    pub compact_disabled: bool,
    /// Shown on the disabled button.
    pub compact_disabled_reason: Option<String>,
    pub dismiss_label: String,
}

/// What the banner decides from.
#[derive(Debug, Clone, Copy)]
pub struct ResumeCompactionInput<'a> {
    pub thread_id: &'a str,
    pub driver: Option<Driver>,
    pub context: Option<&'a ContextWindowSnapshot>,
    pub dismissed_keys: &'a std::collections::BTreeSet<String>,
    /// The resume dialog was told never to ask again for this instance.
    pub instance_dismissed: bool,
    pub native_dismissed: bool,
    pub pending_user_input: bool,
    pub running: bool,
    /// The context meter's compact button; `None` when the provider cannot
    /// compact.
    pub compact: Option<&'a CompactContextButton>,
    pub now_ms: i64,
}

pub fn resume_compaction_banner(
    input: &ResumeCompactionInput<'_>,
) -> Option<ResumeCompactionBanner> {
    let context = input.context?;
    let key = format!("{}:{}", input.thread_id, context.updated_at_ms);
    if input.dismissed_keys.contains(&key)
        || input.instance_dismissed
        || input.native_dismissed
        || input.pending_user_input
        || input.running
        || !should_offer_resume_compaction(
            input.driver,
            Some(context.used_tokens),
            Some(context.updated_at_ms),
            input.now_ms,
        )
    {
        return None;
    }
    let (compact_disabled, compact_disabled_reason) = match input.compact {
        Some(compact) => (compact.disabled, compact.disabled_reason.clone()),
        None => (
            true,
            Some("Compaction is unavailable for this provider".to_owned()),
        ),
    };
    Some(ResumeCompactionBanner {
        key,
        title: "Resume with less context".into(),
        description: format!(
            "{} tokens from earlier",
            format_context_window_tokens(Some(context.used_tokens))
        ),
        compact_label: "Compact".into(),
        compact_disabled,
        compact_disabled_reason,
        dismiss_label: "Keep full history".into(),
    })
}

impl crate::state::Snapshot {
    /// The open thread's resume-compaction offer, given its composer.
    pub fn resume_compaction_banner(
        &self,
        thread: &agent_domain::ThreadId,
        composer: &super::view::ComposerView,
        now_ms: i64,
    ) -> Option<ResumeCompactionBanner> {
        let state = self.thread_state(thread)?;
        let (_, draft) = super::view::composer_draft(self, Some(thread));
        let context = latest_context_window(state);
        let pending_user_input = state.requests.iter().any(|request| {
            request.status == RequestStatus::Pending
                && matches!(request.body, RequestBody::Questions { .. })
        });
        resume_compaction_banner(&ResumeCompactionInput {
            thread_id: thread.as_str(),
            driver: Some(draft.driver),
            context: context.as_ref(),
            dismissed_keys: &self.resume_compaction_dismissals,
            instance_dismissed: self
                .preferences
                .resume_compaction_dismissed
                .contains(&draft.instance_id),
            native_dismissed: has_dismissed_resume_compaction(state),
            pending_user_input,
            running: state.active_run().is_some(),
            compact: composer
                .context_meter
                .as_ref()
                .and_then(|meter| meter.compact.as_ref()),
            now_ms,
        })
    }
}

/// Holds the meter's slot while a started thread's detail loads, unless its
/// provider is known not to report context usage.
pub fn should_reserve_context_window_meter(
    meter_enabled: bool,
    detail_loading: bool,
    thread_started: bool,
    provider_reports_context_window: Option<bool>,
) -> bool {
    meter_enabled
        && detail_loading
        && thread_started
        && provider_reports_context_window != Some(false)
}

#[cfg(test)]
mod tests;
