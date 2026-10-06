use super::*;
use crate::commands::build::{answers_command, dispatch};
use crate::commands::outbox::{PendingCommand, Request as OutboxRequest};
use crate::sync::fixtures::*;
use agent_domain::{
    Answer, Answers, CommandId, Json, QuestionOption, RunAttemptId, RuntimeRequestId, Timestamp,
};
use std::collections::BTreeMap;

fn question(id: &str, header: &str, text: &str, labels: &[&str], multiple: bool) -> Question {
    Question {
        required: true,
        id: id.into(),
        header: header.into(),
        question: text.into(),
        multiple,
        options: labels
            .iter()
            .map(|label| QuestionOption {
                label: (*label).into(),
                description: Some((*label).into()),
            })
            .collect(),
    }
}

fn single_select() -> Question {
    question(
        "scope",
        "Scope",
        "What should the plan target first?",
        &["Orchestration-first"],
        false,
    )
}

fn multi_select() -> Question {
    question(
        "areas",
        "Areas",
        "Which areas should this change cover?",
        &["Server", "Web"],
        true,
    )
}

fn compat() -> Question {
    question(
        "compat",
        "Compat",
        "How strict should compatibility be?",
        &["Keep current envelope"],
        false,
    )
}

fn draft(question_id: &str, selected: &[&str], custom: &str) -> QuestionDraft {
    QuestionDraft {
        question_id: question_id.into(),
        selected_option_values: selected.iter().map(|value| (*value).into()).collect(),
        custom_answer: custom.into(),
        ..QuestionDraft::default()
    }
}

fn text(value: &str) -> Option<ResolvedAnswer> {
    Some(ResolvedAnswer::Text {
        value: value.into(),
    })
}

fn choices(values: &[&str]) -> Option<ResolvedAnswer> {
    Some(ResolvedAnswer::Choices {
        values: values.iter().map(|value| (*value).into()).collect(),
    })
}

fn answer(question_id: &str, values: &[&str]) -> QuestionAnswer {
    QuestionAnswer {
        question_id: question_id.into(),
        values: values.iter().map(|value| (*value).into()).collect(),
    }
}

#[test]
fn prefers_a_custom_answer_over_selected_options() {
    let draft = draft(
        "scope",
        &["Orchestration-first"],
        "Keep the existing envelope for one release",
    );
    assert_eq!(
        resolve_question_answer(&single_select(), Some(&draft)),
        text("Keep the existing envelope for one release")
    );
}

#[test]
fn falls_back_to_the_selected_option_for_single_select_questions() {
    let draft = draft("scope", &["Orchestration-first"], "");
    assert_eq!(
        resolve_question_answer(&single_select(), Some(&draft)),
        text("Orchestration-first")
    );
}

#[test]
fn returns_all_selected_labels_for_multi_select_questions() {
    let draft = draft("areas", &["Server", "Web"], "");
    assert_eq!(
        resolve_question_answer(&multi_select(), Some(&draft)),
        choices(&["Server", "Web"])
    );
}

#[test]
fn clears_the_preset_selection_when_a_custom_answer_is_entered() {
    let selected = draft("areas", &["Server", "Web"], "");
    assert_eq!(
        set_question_custom_answer(&multi_select(), Some(&selected), "doesn't matter"),
        draft("areas", &[], "doesn't matter")
    );
}

#[test]
fn toggles_options_for_multi_select_questions() {
    assert_eq!(
        toggle_question_option(&multi_select(), None, "Server"),
        draft("areas", &["Server"], "")
    );
    assert_eq!(
        toggle_question_option(
            &multi_select(),
            Some(&draft("areas", &["Server", "Web"], "")),
            "Server"
        ),
        draft("areas", &["Web"], "")
    );
}

#[test]
fn returns_a_canonical_answer_map_for_complete_prompts() {
    let drafts = [
        draft("scope", &["Orchestration-first"], ""),
        draft(
            "compat",
            &[],
            "Keep the current envelope for one release window",
        ),
    ];
    assert_eq!(
        build_question_answers(&[single_select(), compat()], &drafts),
        Some(vec![
            answer("scope", &["Orchestration-first"]),
            answer(
                "compat",
                &["Keep the current envelope for one release window"]
            ),
        ])
    );
}

#[test]
fn returns_arrays_for_answered_multi_select_prompts() {
    assert_eq!(
        build_question_answers(&[multi_select()], &[draft("areas", &["Server", "Web"], "")]),
        Some(vec![answer("areas", &["Server", "Web"])])
    );
}

#[test]
fn returns_null_when_any_question_is_unanswered() {
    assert_eq!(build_question_answers(&[single_select()], &[]), None);
}

fn two_questions() -> [Question; 2] {
    [single_select(), compat()]
}

#[test]
fn counts_only_answered_questions() {
    let drafts = [draft("scope", &["Orchestration-first"], "")];
    assert_eq!(count_answered_questions(&two_questions(), &drafts), 1);
}

#[test]
fn finds_the_first_unanswered_question() {
    let drafts = [draft("scope", &["Orchestration-first"], "")];
    assert_eq!(
        first_unanswered_question_index(&two_questions(), &drafts),
        1
    );
}

#[test]
fn returns_the_last_question_index_when_all_answers_are_complete() {
    let drafts = [
        draft("scope", &["Orchestration-first"], ""),
        draft("compat", &[], "Keep it for one release window"),
    ];
    assert_eq!(
        first_unanswered_question_index(&two_questions(), &drafts),
        1
    );
}

#[test]
fn derives_the_active_question_and_advancement_state() {
    let drafts = [draft("scope", &["Orchestration-first"], "")];
    assert_eq!(
        question_progress(&two_questions(), &drafts, 0),
        QuestionProgress {
            question_index: 0,
            active_question_id: Some("scope".into()),
            selected_option_values: vec!["Orchestration-first".into()],
            custom_answer: String::new(),
            resolved_answer: text("Orchestration-first"),
            using_custom_answer: false,
            answered_question_count: 1,
            is_last_question: false,
            is_complete: false,
            can_advance: true,
        }
    );
}

#[test]
fn treats_multi_select_questions_as_answered_when_they_have_selected_options() {
    let progress = question_progress(
        &[multi_select()],
        &[draft("areas", &["Server", "Web"], "")],
        0,
    );
    assert_eq!(progress.selected_option_values, ["Server", "Web"]);
    assert_eq!(progress.resolved_answer, choices(&["Server", "Web"]));
    assert!(progress.can_advance);
    assert!(progress.is_complete);
}

fn spec() -> Question {
    question("spec", "Spec", "Provide a spec", &[], false)
}

fn with_attachments(mut draft: QuestionDraft, blocked: bool) -> QuestionDraft {
    draft.attachment_count = 1;
    draft.attachments_blocked = blocked;
    draft
}

#[test]
fn accepts_attachment_only_answers_after_every_upload_finishes() {
    assert_eq!(
        build_question_answers(
            &[spec()],
            &[with_attachments(draft("spec", &[], ""), false)]
        ),
        Some(vec![answer("spec", &[""])])
    );
    assert_eq!(
        build_question_answers(&[spec()], &[with_attachments(draft("spec", &[], ""), true)]),
        None
    );
    // An attachment-only answer is answered but does not stop the search for
    // the first unanswered question.
    let drafts = [with_attachments(draft("spec", &[], ""), false)];
    assert_eq!(count_answered_questions(&[spec(), compat()], &drafts), 1);
    assert_eq!(
        first_unanswered_question_index(&[spec(), compat()], &drafts),
        0
    );
}

#[test]
fn keeps_the_thread_draft_when_nothing_was_typed_into_the_answer() {
    assert_eq!(
        carry_displaced_custom_answer_into_prompt("draft", ""),
        "draft"
    );
    assert_eq!(
        carry_displaced_custom_answer_into_prompt("draft", "   "),
        "draft"
    );
}

#[test]
fn moves_the_typed_answer_into_an_empty_thread_draft() {
    assert_eq!(
        carry_displaced_custom_answer_into_prompt("", "also rename the flag "),
        "also rename the flag"
    );
}

#[test]
fn appends_the_typed_answer_after_an_existing_thread_draft() {
    assert_eq!(
        carry_displaced_custom_answer_into_prompt("first half\n", "second half"),
        "first half\n\nsecond half"
    );
}

fn runtime() -> Question {
    question(
        "runtime",
        "Runtime",
        "Which runtime should be used?",
        &["Go", "Node.js"],
        false,
    )
}

fn data_scope() -> Question {
    question(
        "scope",
        "Scope",
        "Which data should be collected?",
        &["Orders", "Listings"],
        true,
    )
}

#[test]
fn replaces_single_select_options_and_toggles_multi_select_options() {
    assert_eq!(
        toggle_question_option(&runtime(), Some(&draft("runtime", &["Go"], "")), "Node.js"),
        draft("runtime", &["Node.js"], "")
    );
    let orders = toggle_question_option(&data_scope(), None, "Orders");
    let orders_and_listings = toggle_question_option(&data_scope(), Some(&orders), "Listings");
    assert_eq!(
        orders_and_listings,
        draft("scope", &["Orders", "Listings"], "")
    );
    assert_eq!(
        toggle_question_option(&data_scope(), Some(&orders_and_listings), "Orders"),
        draft("scope", &["Listings"], "")
    );
    let padded = toggle_question_option(&data_scope(), None, "  Orders  ");
    assert_eq!(padded, draft("scope", &["Orders"], ""));
    assert_eq!(
        toggle_question_option(&data_scope(), Some(&padded), "  Orders  "),
        draft("scope", &[], "")
    );
}

#[test]
fn builds_array_answers_for_multi_select_questions() {
    let drafts = [
        draft("runtime", &["Go"], ""),
        draft("scope", &["Orders", "Listings"], ""),
    ];
    assert_eq!(
        build_question_answers(&[runtime(), data_scope()], &drafts),
        Some(vec![
            answer("runtime", &["Go"]),
            answer("scope", &["Orders", "Listings"])
        ])
    );
}

#[test]
fn clears_selected_options_while_a_custom_answer_is_active() {
    assert_eq!(
        set_question_custom_answer(
            &data_scope(),
            Some(&draft("scope", &["Orders", "Listings"], "")),
            "Orders first"
        ),
        draft("scope", &[], "Orders first")
    );
}

#[test]
fn matches_selected_chips_against_normalized_option_labels() {
    assert!(is_question_option_selected(
        &data_scope(),
        Some(&draft("scope", &["Orders"], "")),
        "  Orders  "
    ));
    assert!(!is_question_option_selected(
        &data_scope(),
        Some(&draft("scope", &["Orders"], "Orders first")),
        "  Orders  "
    ));
}

#[test]
fn accepts_ready_attachment_only_answers_while_preserving_selected_options() {
    let question = question("q", "Spec", "Provide a specification", &["Yes"], false);
    let resolve =
        |draft: QuestionDraft| build_question_answers(std::slice::from_ref(&question), &[draft]);
    assert_eq!(
        resolve(with_attachments(draft("q", &[], ""), false)),
        Some(vec![answer("q", &[""])])
    );
    assert_eq!(
        resolve(with_attachments(draft("q", &["Yes"], ""), false)),
        Some(vec![answer("q", &["Yes"])])
    );
    assert_eq!(resolve(with_attachments(draft("q", &[], ""), true)), None);
}

#[test]
fn caps_a_tall_portrait_viewport() {
    assert_eq!(pending_input_max_height(932.0, 0.0, 103.0, 94.0), 560.0);
}

#[test]
fn subtracts_the_keyboard_while_editing_a_custom_answer() {
    assert_eq!(pending_input_max_height(932.0, 336.0, 103.0, 94.0), 387.0);
}

#[test]
fn keeps_the_fixed_action_area_usable_in_a_short_keyboard_open_viewport() {
    assert_eq!(pending_input_max_height(375.0, 240.0, 44.0, 94.0), 160.0);
}

fn request(id: &str, body: RequestBody, capability: ResponseCapability, at_ms: i64) -> Request {
    Request {
        owner_path: vec![],
        id: RuntimeRequestId::new(id).unwrap(),
        attempt: RunAttemptId::new("attempt").unwrap(),
        native_key: id.into(),
        body,
        capability,
        status: RequestStatus::Pending,
        decision: None,
        answers: None,
        attachments: BTreeMap::new(),
        created_at: Timestamp::from_millis(at_ms).unwrap(),
        resolved_at: None,
    }
}

fn next_question() -> Question {
    Question {
        required: true,
        id: "next".into(),
        header: "Next".into(),
        question: "What should happen next?".into(),
        multiple: false,
        options: vec![QuestionOption {
            label: "Continue".into(),
            description: Some("Resume work".into()),
        }],
    }
}

fn async_question(capability: ResponseCapability) -> State {
    let mut state = thread_state("Thread");
    state.requests.push(request(
        "async-question",
        RequestBody::Questions {
            questions: vec![next_question()],
        },
        capability,
        0,
    ));
    state
}

#[test]
fn keeps_message_responses_available_after_the_originating_runtime_exits() {
    let state = async_question(ResponseCapability::Message);
    assert!(state.native_sessions.is_empty());
    let view = requests_view(&state, &Outbox::default(), &[], 0)
        .questions
        .unwrap();
    assert_eq!(view.request_id, "async-question");
    assert!(view.dismissible && view.can_respond);
    assert_eq!(
        view.questions,
        [QuestionView {
            id: "next".into(),
            header: "Next".into(),
            question: "What should happen next?".into(),
            multiple: false,
            hint: None,
            custom_answer: String::new(),
            options: vec![QuestionOptionView {
                label: "Continue".into(),
                value: "Continue".into(),
                description: Some("Resume work".into()),
                selected: false,
                shortcut: Some(1),
            }],
        }]
    );
}

#[test]
fn only_marks_asynchronous_questions_as_dismissible() {
    let state = async_question(ResponseCapability::Live);
    let view = requests_view(&state, &Outbox::default(), &[], 0);
    assert!(!view.questions.unwrap().dismissible);
}

#[test]
fn removes_answered_requests_from_the_composer_while_retaining_their_answers() {
    let mut state = async_question(ResponseCapability::Message);
    let answers = Answers::from([("next".into(), Answer::Text("  continue  ".into()))]);
    state.requests[0].status = RequestStatus::Resolved;
    state.requests[0].answers = Some(answers.clone());
    let view = requests_view(&state, &Outbox::default(), &[], 0);
    assert_eq!(view.questions, None);
    assert_eq!(view.question_request_count, 0);
    assert_eq!(state.requests[0].answers, Some(answers));
}

fn approval(
    id: &str,
    kind: &str,
    title: &str,
    options: Vec<ApprovalOption>,
    at_ms: i64,
) -> Request {
    request(
        id,
        RequestBody::Approval {
            kind: kind.into(),
            title: title.into(),
            detail: None,
            options,
            input: Json(serde_json::json!({ "message": "Allow Linear to read issues?" })),
        },
        ResponseCapability::Live,
        at_ms,
    )
}

fn provider_options() -> Vec<ApprovalOption> {
    [
        (ApprovalDecision::Accept, "Approve"),
        (ApprovalDecision::AcceptForSession, "Approve for session"),
        (ApprovalDecision::Decline, "Decline"),
        (ApprovalDecision::Cancel, "Cancel"),
    ]
    .into_iter()
    .map(|(decision, label)| option(decision, label))
    .collect()
}

fn labels(actions: &[ApprovalAction]) -> Vec<(&str, &str, ApprovalTone, bool)> {
    actions
        .iter()
        .map(|action| {
            (
                action.label.as_str(),
                action.decision.as_str(),
                action.tone,
                action.enabled,
            )
        })
        .collect()
}

#[test]
fn shows_the_oldest_approval_with_its_counter_and_default_choices() {
    let mut state = thread_state("Thread");
    state.requests.extend([
        approval("newer", "file-change", "file-change", provider_options(), 2),
        approval("older", "command", "git push", provider_options(), 1),
    ]);
    let view = requests_view(&state, &Outbox::default(), &[], 0);
    assert_eq!(view.approval_count, 2);
    let approval = view.approval.unwrap();
    assert_eq!(approval.request_id, "older");
    assert_eq!(approval.label, "Command approval");
    assert_eq!(approval.detail_accessibility_label, "Command");
    assert_eq!(approval.detail, "git push");
    assert!(approval.detail_monospace);
    assert_eq!(approval.counter.as_deref(), Some("1/2"));
    use ApprovalTone::*;
    assert_eq!(
        labels(&approval.actions),
        [
            ("Decline", "decline", Secondary, true),
            ("Approve", "accept", Primary, true)
        ]
    );
    assert_eq!(
        labels(&approval.more_actions),
        [
            ("Cancel", "cancel", Secondary, true),
            (
                "Always allow this session",
                "acceptForSession",
                Secondary,
                true
            )
        ]
    );
    assert_eq!(approval.card_title, "command");
    assert_eq!(
        labels(&approval.card_actions),
        [
            ("Allow once", "accept", Primary, true),
            ("Allow session", "acceptForSession", Secondary, true),
            ("Decline", "decline", Danger, true)
        ]
    );
}

#[test]
fn defaults_offer_only_the_decisions_the_request_accepts() {
    let offered = vec![
        option(ApprovalDecision::Accept, "Yes"),
        option(ApprovalDecision::Decline, "No"),
    ];
    let request = approval("only", "file-read", "file-read", offered, 0);
    let approval = approval_view(&request, 1, false).unwrap();
    assert_eq!(approval.label, "File read approval");
    assert_eq!(approval.detail, "File read approval");
    assert_eq!(approval.counter, None);
    assert!(approval.more_actions.is_empty());
    let card: Vec<_> = approval
        .card_actions
        .iter()
        .map(|action| action.label.as_str())
        .collect();
    assert_eq!(card, ["Allow once", "Decline"]);
}

#[test]
fn app_access_requests_show_the_app_and_its_own_choices() {
    let offered = vec![
        option(ApprovalDecision::Accept, "Allow"),
        option(
            ApprovalDecision::AcceptForSession,
            "Allow and don't ask again",
        ),
        option(ApprovalDecision::Decline, "Deny"),
    ];
    let request = approval("app", "mcp-elicitation", "Linear", offered, 0);
    let approval = approval_view(&request, 1, false).unwrap();
    assert_eq!(approval.label, "App access approval");
    assert_eq!(approval.app_name.as_deref(), Some("Linear"));
    assert_eq!(approval.detail, "Allow Linear to read issues?");
    assert!(!approval.detail_monospace);
    assert_eq!(approval.card_title, "Linear");
    let desktop: Vec<_> = approval
        .actions
        .iter()
        .chain(&approval.more_actions)
        .map(|action| action.label.as_str())
        .collect();
    assert_eq!(desktop, ["Allow", "Deny", "Allow and don't ask again"]);
    let card: Vec<_> = approval
        .card_actions
        .iter()
        .map(|action| (action.label.as_str(), action.tone))
        .collect();
    assert_eq!(
        card,
        [
            ("Allow", ApprovalTone::Primary),
            ("Allow and don't ask again", ApprovalTone::Secondary),
            ("Deny", ApprovalTone::Danger)
        ]
    );
}

#[test]
fn an_approval_without_its_provider_process_cannot_be_answered() {
    let mut request = approval("gone", "command", "ls", provider_options(), 0);
    request.capability = ResponseCapability::NotResumable;
    let approval = approval_view(&request, 1, false).unwrap();
    assert!(!approval.can_respond);
    assert_eq!(
        approval.detail,
        "Provider process is gone — interrupt or restart the run to respond."
    );
    assert_eq!(
        approval.unavailable_notice.as_deref(),
        Some(REQUEST_UNAVAILABLE_NOTICE)
    );
    assert!(approval.actions.iter().all(|action| !action.enabled));
    assert!(approval.more_actions.iter().all(|action| action.enabled));
    assert!(approval.card_actions.iter().all(|action| !action.enabled));
}

#[test]
fn a_sent_reply_marks_its_request_responding() {
    let mut state = async_question(ResponseCapability::Live);
    state
        .requests
        .push(approval("approval", "command", "ls", provider_options(), 1));
    let sent = Outbox {
        entries: vec![PendingCommand::new(
            thread_id(),
            OutboxRequest::Dispatch(Box::new(dispatch(
                thread_id(),
                CommandId::new("reply").unwrap(),
                answers_command(
                    RuntimeRequestId::new("async-question").unwrap(),
                    Answers::new(),
                    BTreeMap::new(),
                ),
            ))),
            at(),
        )],
    };
    let drafts = [draft("next", &["Continue"], "")];
    let view = requests_view(&state, &sent, &drafts, 0);
    let questions = view.questions.unwrap();
    assert!(questions.responding);
    assert!(!questions.submit_enabled);
    assert!(!view.approval.unwrap().responding);
    let idle = requests_view(&state, &Outbox::default(), &drafts, 0)
        .questions
        .unwrap();
    assert!(idle.submit_enabled);
    assert_eq!(idle.answers, Some(vec![answer("next", &["Continue"])]));
}

#[test]
fn a_question_request_names_its_progress_options_and_shortcuts() {
    let mut state = thread_state("Thread");
    let mut many = question(
        "many",
        "Many",
        "Pick any",
        &["1", "2", "3", "4", "5", "6", "7", "8", "9", "10"],
        true,
    );
    many.options[0].description = Some("First option".into());
    state.requests.push(request(
        "questions",
        RequestBody::Questions {
            questions: vec![single_select(), many],
        },
        ResponseCapability::Live,
        0,
    ));
    let drafts = [
        draft("scope", &["Orchestration-first"], ""),
        draft("many", &["2"], ""),
    ];
    let view = requests_view(&state, &Outbox::default(), &drafts, 5)
        .questions
        .unwrap();
    assert_eq!(view.counter.as_deref(), Some("2/2"));
    assert_eq!(view.count_label, "2 questions");
    assert_eq!(
        view.expand_accessibility_label,
        "Expand user input, 2 questions"
    );
    assert!(view.progress.is_last_question && view.progress.is_complete);
    let active = view.active.unwrap();
    assert_eq!(active.hint.as_deref(), Some("Select one or more options."));
    let options: Vec<_> = active
        .options
        .iter()
        .map(|option| {
            (
                option.description.as_deref(),
                option.selected,
                option.shortcut,
            )
        })
        .collect();
    assert_eq!(options[0], (Some("First option"), false, Some(1)));
    assert_eq!(options[1], (None, true, Some(2)));
    assert_eq!(options[9], (None, false, None));
    assert!(!view.dismissible);
    assert_eq!(view.unavailable_notice, None);
}
