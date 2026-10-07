use super::*;
use crate::sync::fixtures::{at, command_item, run, thread_state};
use agent_domain::{
    Answer, Attempt, AttemptStatus, ContextUsage, Request, ResponseCapability, RunAttemptId, RunId,
    RunStatus, RuntimeRequestId, Timestamp,
};

fn claude(instance: &str, group: &str, enabled: bool) -> CompactionProvider {
    CompactionProvider {
        instance_id: instance.into(),
        driver: Driver::Claude,
        continuation_group_key: Some(group.into()),
        enabled,
        available: true,
        ready: true,
        errored: false,
        supports_compact: true,
    }
}

#[test]
fn rejects_a_fallback_in_a_different_locked_continuation_group() {
    let providers = [
        claude("claude_original", "claude:home:/original", false),
        claude("claude_other", "claude:home:/other", true),
    ];
    assert!(!has_available_compaction_provider(
        &providers,
        Driver::Claude,
        Some("claude_original"),
        Some("claude_original"),
    ));
}

#[test]
fn accepts_an_enabled_fallback_in_the_locked_continuation_group() {
    let providers = [
        claude("claude_original", "claude:home:/original", false),
        claude("claude_fallback", "claude:home:/original", true),
    ];
    assert!(has_available_compaction_provider(
        &providers,
        Driver::Claude,
        Some("claude_original"),
        Some("claude_original"),
    ));
}

#[test]
fn falls_back_to_the_selected_model_slug_when_model_metadata_is_unavailable() {
    assert_eq!(
        context_window_model_display_name("custom-model", None),
        "custom-model"
    );
    assert_eq!(
        context_window_model_display_name("gpt-5.6-sol", Some("5.6 Sol")),
        "5.6 Sol"
    );
}

#[test]
fn describes_compaction_in_terms_of_the_selected_model() {
    assert_eq!(
        context_window_compaction_message(Some("GPT-5.6 Sol"), None),
        "Context for GPT-5.6 Sol compacts automatically when needed."
    );
}

#[test]
fn uses_neutral_copy_when_the_model_is_unavailable() {
    assert_eq!(
        context_window_compaction_message(None, None),
        "Context compacts automatically when needed."
    );
}

#[test]
fn shows_the_configured_auto_compaction_threshold() {
    assert_eq!(
        context_window_compaction_message(Some("Claude Sonnet 5"), Some(300_000)),
        "Compacts automatically at 300,000 tokens."
    );
}

fn millis(iso: &str) -> Option<i64> {
    Some(Timestamp::parse(iso).unwrap().millis())
}
const NOW: &str = "2026-08-24T12:00:00.000Z";

#[test]
fn matches_claudes_old_session_age_and_context_thresholds() {
    assert!(should_offer_resume_compaction(
        Some(Driver::Claude),
        Some(100_000),
        millis("2026-08-24T10:50:00.000Z"),
        millis(NOW).unwrap(),
    ));
}

#[test]
fn does_not_prompt_for_recent_or_smaller_sessions() {
    let now = millis(NOW).unwrap();
    assert!(!should_offer_resume_compaction(
        Some(Driver::Claude),
        Some(99_999),
        millis("2026-08-24T10:00:00.000Z"),
        now,
    ));
    assert!(!should_offer_resume_compaction(
        Some(Driver::Claude),
        Some(200_000),
        millis("2026-08-24T10:51:00.000Z"),
        now,
    ));
}

#[test]
fn does_not_show_claudes_resume_prompt_for_another_provider() {
    assert!(!should_offer_resume_compaction(
        Some(Driver::Codex),
        Some(300_000),
        millis("2026-08-24T09:00:00.000Z"),
        millis(NOW).unwrap(),
    ));
}

fn resolved(status: RequestStatus, body: RequestBody, answers: &[(&str, Answer)]) -> Request {
    Request {
        owner_path: vec![],
        id: RuntimeRequestId::new("request").unwrap(),
        attempt: RunAttemptId::new("attempt").unwrap(),
        native_key: "request".into(),
        body,
        capability: ResponseCapability::Live,
        status,
        decision: None,
        answers: Some(
            answers
                .iter()
                .map(|(question, answer)| (question.to_string(), answer.clone()))
                .collect(),
        ),
        attachments: Default::default(),
        created_at: at(),
        resolved_at: None,
    }
}

fn questions() -> RequestBody {
    RequestBody::Questions { questions: vec![] }
}

fn dismissed(requests: Vec<Request>) -> bool {
    let mut state = thread_state("Thread");
    state.requests = requests;
    has_dismissed_resume_compaction(&state)
}

fn never() -> Answer {
    Answer::Text(RESUME_COMPACTION_NEVER_ANSWER.into())
}

#[test]
fn recognizes_the_native_resume_dialogs_permanent_dismissal() {
    assert!(dismissed(vec![resolved(
        RequestStatus::Resolved,
        questions(),
        &[(
            "This session is 2h 0m old and uses 250,000 tokens. Compact it before continuing?",
            never()
        )]
    )]));
    assert!(dismissed(vec![resolved(
        RequestStatus::Resolved,
        questions(),
        &[(
            "This session is 45m old and uses 1,250,000 tokens. Compact it before continuing?",
            Answer::Choices(vec![RESUME_COMPACTION_NEVER_ANSWER.into()])
        )]
    )]));
}

#[test]
fn ignores_the_same_answer_on_an_unrelated_question() {
    assert!(!dismissed(vec![resolved(
        RequestStatus::Resolved,
        questions(),
        &[("Show this setup reminder?", never())]
    )]));
}

#[test]
fn ignores_unrelated_questions_that_end_with_claudes_compaction_prompt() {
    assert!(!dismissed(vec![resolved(
        RequestStatus::Resolved,
        questions(),
        &[(
            "The build cache is large. Compact it before continuing?",
            never()
        )]
    )]));
    for question in [
        "This session is 2h old and uses 250,000 tokens. Compact it before continuing?",
        "This session is 2h 0m old and uses 2500,000 tokens. Compact it before continuing?",
        "This session is m old and uses 250 tokens. Compact it before continuing?",
    ] {
        assert!(!is_resume_compaction_question(question), "{question}");
    }
}

#[test]
fn ignores_pending_questions_and_answers_that_are_not_resolved() {
    let question =
        "This session is 2h 0m old and uses 250,000 tokens. Compact it before continuing?";
    assert!(!dismissed(vec![resolved(
        RequestStatus::Pending,
        questions(),
        &[(question, never())]
    )]));
    let mut unanswered = resolved(RequestStatus::Resolved, questions(), &[]);
    unanswered.answers = None;
    assert!(!dismissed(vec![unanswered]));
}

#[test]
fn holds_the_meters_slot_while_a_started_threads_detail_loads() {
    assert!(should_reserve_context_window_meter(
        true,
        true,
        true,
        Some(true)
    ));
}

#[test]
fn reserves_nothing_once_the_detail_is_in() {
    assert!(!should_reserve_context_window_meter(
        true,
        false,
        true,
        Some(true)
    ));
}

#[test]
fn reserves_nothing_for_a_thread_that_never_ran_a_turn() {
    assert!(!should_reserve_context_window_meter(
        true,
        true,
        false,
        Some(true)
    ));
}

#[test]
fn reserves_while_the_threads_provider_is_not_in_the_catalog_yet() {
    assert!(should_reserve_context_window_meter(true, true, true, None));
}

#[test]
fn reserves_nothing_for_a_provider_that_does_not_stream_usage() {
    assert!(!should_reserve_context_window_meter(
        true,
        true,
        true,
        Some(false)
    ));
}

#[test]
fn reserves_nothing_while_the_meter_is_switched_off() {
    assert!(!should_reserve_context_window_meter(
        false,
        true,
        true,
        Some(true)
    ));
}

#[test]
fn formats_token_counts_like_the_meter() {
    let format = |value| format_context_window_tokens(Some(value));
    assert_eq!(format_context_window_tokens(None), "0");
    assert_eq!(format(999), "999");
    assert_eq!(format(1_000), "1k");
    assert_eq!(format(1_250), "1.3k");
    assert_eq!(format(9_960), "10k");
    assert_eq!(format(45_499), "45k");
    assert_eq!(format(45_500), "46k");
    assert_eq!(format(1_000_000), "1m");
    assert_eq!(format(1_550_000), "1.6m");
}

fn attempt(id: &str, usage: Option<ContextUsage>) -> Attempt {
    Attempt {
        id: RunAttemptId::new(id).unwrap(),
        run: RunId::new("run").unwrap(),
        ordinal: 1,
        status: AttemptStatus::Completed,
        native_thread: None,
        native_turn: None,
        native_head: None,
        accepted: true,
        usage: None,
        context_usage: usage,
        turn_usage: None,
        usage_accumulator: None,
        usage_observed: false,
        rejected_limits: Default::default(),
        started_at: at(),
        completed_at: None,
    }
}

#[test]
fn prefers_the_newest_attempt_usage_then_the_latest_compaction() {
    let mut state = thread_state("Thread");
    assert_eq!(latest_context_window(&state), None);
    state.runs.push(run("run", 1, RunStatus::Completed));
    let mut compaction = command_item("compaction", 1);
    compaction.run = Some(RunId::new("run").unwrap());
    compaction.kind = ItemKind::Compaction {
        before: Some(180_000),
        after: Some(42_000),
    };
    state.items.push(compaction);
    let compacted = latest_context_window(&state).unwrap();
    assert_eq!(
        (
            compacted.used_tokens,
            compacted.total_processed_tokens,
            compacted.max_tokens,
            compacted.used_percentage
        ),
        (42_000, Some(180_000), None, None)
    );
    state.attempts.extend([
        attempt(
            "older",
            Some(ContextUsage {
                used_tokens: 1,
                max_tokens: Some(10),
                auto_compact_threshold: None,
            }),
        ),
        attempt(
            "newer",
            Some(ContextUsage {
                used_tokens: 150_000,
                max_tokens: Some(200_000),
                auto_compact_threshold: Some(180_000),
            }),
        ),
        attempt("no-report", None),
    ]);
    let live = latest_context_window(&state).unwrap();
    assert_eq!(
        (
            live.used_tokens,
            live.max_tokens,
            live.remaining_tokens,
            live.used_percentage,
            live.remaining_percentage,
            live.auto_compact_threshold
        ),
        (
            150_000,
            Some(200_000),
            Some(50_000),
            Some(75.0),
            Some(25.0),
            Some(180_000)
        )
    );
}

#[test]
fn the_meter_shows_percentage_with_a_known_window_and_tokens_otherwise() {
    let known = snapshot(190_000, Some(200_000), Some(2_000_000), None, 0);
    let meter = context_window_meter(
        &known,
        Some("GPT-5.6 Sol"),
        &CompactControl {
            available: true,
            disabled: true,
            disabled_reason: Some("Compacting is unavailable right now".into()),
        },
    );
    assert_eq!(meter.accessibility_label, "Context window 95% used");
    assert_eq!(meter.used_percentage_label.as_deref(), Some("95%"));
    assert_eq!(meter.tokens_label, "190k/200k");
    assert_eq!(meter.progress, Some(95.0));
    assert!(meter.overloaded);
    assert_eq!(meter.total_processed_label.as_deref(), Some("2m"));
    assert_eq!(
        meter.compaction_message.as_deref(),
        Some("Context for GPT-5.6 Sol compacts automatically when needed.")
    );
    assert_eq!(
        meter.compact,
        Some(CompactContextButton {
            label: "Compact context".into(),
            disabled: true,
            disabled_reason: Some("Compacting is unavailable right now".into()),
        })
    );
    let small = context_window_meter(
        &snapshot(4_321, Some(100_000), None, None, 0),
        None,
        &CompactControl::default(),
    );
    assert_eq!(small.used_percentage_label.as_deref(), Some("4.3%"));
    assert_eq!(small.compact, None);
    let unknown = context_window_meter(
        &snapshot(42_000, None, None, None, 0),
        None,
        &CompactControl::default(),
    );
    assert_eq!(
        unknown.accessibility_label,
        "Context window 42k tokens used"
    );
    assert_eq!(unknown.tokens_label, "42k");
    assert_eq!(unknown.used_percentage_label, None);
    assert_eq!(unknown.progress, None);
    assert!(!unknown.overloaded);
}
