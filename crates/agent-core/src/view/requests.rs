//! Pending approvals and questions: the composer drawer on desktop and the
//! request cards on mobile, with the device's answer drafts.
use crate::commands::outbox::Outbox;
use crate::js_text::{is_js_space, js_trim};
use crate::state::{Snapshot, answer_draft_key};
use crate::view::quantity;
use crate::view::queue::outbox_commands;
use agent_domain::{
    ApprovalDecision, ApprovalOption, Command, Question, Request, RequestBody, RequestStatus,
    ResponseCapability, RuntimeRequestId, State,
};

pub const APPROVAL_NEEDED: &str = "Approval needed";
pub const MORE_APPROVAL_OPTIONS: &str = "More approval options";
pub const APPROVAL_UNAVAILABLE_DETAIL: &str =
    "Provider process is gone — interrupt or restart the run to respond.";
pub const REQUEST_UNAVAILABLE_NOTICE: &str = "The provider process for this request is no longer available. Interrupt or restart the run to continue.";
pub const USER_INPUT_NEEDED: &str = "User input needed";
pub const USER_INPUT_CARD_TITLE: &str = "Fill in the pending answers";
pub const MULTIPLE_CHOICE_HINT: &str = "Select one or more options.";
pub const SUBMIT_ANSWERS: &str = "Submit answers";
pub const DISMISS_QUESTION: &str = "Dismiss question without answering";
pub const DISMISS_QUESTION_CARD: &str = "Dismiss without answering";
pub const SHOW_QUESTION: &str = "Show the question and its options";
pub const HIDE_QUESTION: &str = "Hide the question and its options";
pub const COLLAPSE_USER_INPUT: &str = "Collapse user input";

/// Reserve for a portrait iPhone keyboard until a real height is observed.
pub const ESTIMATED_KEYBOARD_HEIGHT: f64 = 336.0;
/// The question card's expand and collapse animation.
pub const USER_INPUT_TOGGLE_DURATION_MS: u32 = 220;
const PENDING_USER_INPUT_MAX_HEIGHT: f64 = 560.0;
const PENDING_USER_INPUT_MIN_HEIGHT: f64 = 160.0;
const PENDING_USER_INPUT_VERTICAL_GAP: f64 = 12.0;

/// Pending approvals the user can answer, oldest first.
pub fn pending_approvals(state: &State) -> Vec<&Request> {
    let mut approvals: Vec<&Request> = state
        .requests
        .iter()
        .filter(|request| request.status == RequestStatus::Pending)
        .filter(|request| match &request.body {
            RequestBody::Approval { kind, .. } => {
                kind != "auth_refresh" && kind != "dynamic_tool_call"
            }
            RequestBody::Questions { .. } => false,
        })
        .collect();
    approvals.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    approvals
}

/// Pending question requests, oldest first.
pub fn pending_questions(state: &State) -> Vec<&Request> {
    let mut questions: Vec<&Request> = state
        .requests
        .iter()
        .filter(|request| request.status == RequestStatus::Pending)
        .filter(|request| matches!(request.body, RequestBody::Questions { .. }))
        .collect();
    questions.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    questions
}

/// Only questions answered by a later message can be closed without a reply.
pub fn question_dismissible(request: &Request) -> bool {
    request.capability == ResponseCapability::Message
}

fn questions_of(request: &Request) -> &[Question] {
    match &request.body {
        RequestBody::Questions { questions } => questions,
        RequestBody::Approval { .. } => &[],
    }
}

/// The wire value `Intent::RespondApproval` takes.
pub fn approval_decision_value(decision: ApprovalDecision) -> &'static str {
    match decision {
        ApprovalDecision::Accept => "accept",
        ApprovalDecision::AcceptForSession => "acceptForSession",
        ApprovalDecision::AcceptAlways => "acceptAlways",
        ApprovalDecision::Decline => "decline",
        ApprovalDecision::Cancel => "cancel",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ApprovalTone {
    Primary,
    Secondary,
    Danger,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ApprovalAction {
    pub label: String,
    pub decision: String,
    pub tone: ApprovalTone,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ApprovalView {
    pub request_id: String,
    pub kind: String,
    /// The desktop panel's label, such as "Command approval".
    pub label: String,
    pub detail_accessibility_label: String,
    /// The desktop panel's detail text.
    pub detail: String,
    pub detail_monospace: bool,
    pub app_name: Option<String>,
    /// "1/N" while more than one approval waits.
    pub counter: Option<String>,
    pub can_respond: bool,
    /// A reply from this device is waiting for the Host.
    pub responding: bool,
    /// Desktop buttons: Decline and Approve.
    pub actions: Vec<ApprovalAction>,
    /// Desktop "…" menu entries; the menu opens unless responding.
    pub more_actions: Vec<ApprovalAction>,
    pub card_title: String,
    pub card_detail: Option<String>,
    pub unavailable_notice: Option<String>,
    /// Mobile card buttons.
    pub card_actions: Vec<ApprovalAction>,
}

fn option(decision: ApprovalDecision, label: &str) -> ApprovalOption {
    ApprovalOption {
        label: label.into(),
        decision,
    }
}

fn desktop_defaults() -> Vec<ApprovalOption> {
    vec![
        option(ApprovalDecision::Cancel, "Cancel"),
        option(ApprovalDecision::Decline, "Decline"),
        option(
            ApprovalDecision::AcceptForSession,
            "Always allow this session",
        ),
        option(ApprovalDecision::Accept, "Approve"),
    ]
}

fn mobile_defaults() -> Vec<ApprovalOption> {
    vec![
        option(ApprovalDecision::Accept, "Allow once"),
        option(ApprovalDecision::AcceptForSession, "Allow session"),
        option(ApprovalDecision::Decline, "Decline"),
    ]
}

/// App-access requests carry their own choices; other approvals show the
/// default choices the request accepts.
fn shown_options(
    kind: &str,
    offered: &[ApprovalOption],
    defaults: Vec<ApprovalOption>,
) -> Vec<ApprovalOption> {
    if kind == "mcp-elicitation" && !offered.is_empty() {
        return offered.to_vec();
    }
    if offered.is_empty() {
        return defaults;
    }
    defaults
        .into_iter()
        .filter(|default| {
            offered
                .iter()
                .any(|choice| choice.decision == default.decision)
        })
        .collect()
}

fn approval_label(kind: &str) -> (&'static str, &'static str) {
    match kind {
        "mcp-elicitation" => ("App access approval", "App access request"),
        "command" => ("Command approval", "Command"),
        "file-read" => ("File read approval", "File to read"),
        "permission" => ("App permission approval", "Permission request"),
        _ => ("File change approval", "File change"),
    }
}

fn non_empty(text: Option<&str>) -> Option<String> {
    text.map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

pub fn approval_view(
    request: &Request,
    pending_count: usize,
    responding: bool,
) -> Option<ApprovalView> {
    let RequestBody::Approval {
        kind,
        title,
        detail,
        options,
        input,
    } = &request.body
    else {
        return None;
    };
    let app = kind == "mcp-elicitation";
    let app_name = app.then(|| non_empty(Some(title))).flatten();
    let detail = non_empty(detail.as_deref())
        .or_else(|| {
            app.then(|| non_empty(input.0["message"].as_str()))
                .flatten()
        })
        .or_else(|| {
            (!app && title != kind)
                .then(|| non_empty(Some(title)))
                .flatten()
        });
    let (label, detail_label) = approval_label(kind);
    let can_respond = request.capability == ResponseCapability::Live;
    let action = |option: &ApprovalOption, tone: ApprovalTone, enabled: bool| ApprovalAction {
        label: option.label.clone(),
        decision: approval_decision_value(option.decision).into(),
        tone,
        enabled,
    };
    let desktop = shown_options(kind, options, desktop_defaults());
    let primary = |option: &&ApprovalOption| {
        matches!(
            option.decision,
            ApprovalDecision::Decline | ApprovalDecision::Accept
        )
    };
    Some(ApprovalView {
        request_id: request.id.to_string(),
        kind: kind.clone(),
        label: label.into(),
        detail_accessibility_label: detail_label.into(),
        detail: if can_respond {
            detail.clone().unwrap_or_else(|| label.into())
        } else {
            APPROVAL_UNAVAILABLE_DETAIL.into()
        },
        detail_monospace: !app,
        counter: (pending_count > 1).then(|| format!("1/{pending_count}")),
        can_respond,
        responding,
        actions: desktop
            .iter()
            .filter(primary)
            .map(|option| {
                let tone = if option.decision == ApprovalDecision::Accept {
                    ApprovalTone::Primary
                } else {
                    ApprovalTone::Secondary
                };
                action(option, tone, !responding && can_respond)
            })
            .collect(),
        more_actions: desktop
            .iter()
            .filter(|option| !primary(option))
            .map(|option| action(option, ApprovalTone::Secondary, !responding))
            .collect(),
        card_title: app_name.clone().unwrap_or_else(|| kind.clone()),
        card_detail: detail,
        unavailable_notice: (!can_respond).then(|| REQUEST_UNAVAILABLE_NOTICE.into()),
        card_actions: shown_options(kind, options, mobile_defaults())
            .iter()
            .map(|option| {
                let tone = match option.decision {
                    ApprovalDecision::Accept => ApprovalTone::Primary,
                    ApprovalDecision::Decline => ApprovalTone::Danger,
                    _ => ApprovalTone::Secondary,
                };
                action(option, tone, can_respond && !responding)
            })
            .collect(),
        app_name,
    })
}

/// One question's answer values, as the reply lists them.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct QuestionAnswer {
    pub question_id: String,
    pub values: Vec<String>,
}

/// One question's answer draft on this device.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct QuestionDraft {
    pub question_id: String,
    pub selected_option_values: Vec<String>,
    pub custom_answer: String,
    /// Files attached to the answer.
    pub attachment_count: u32,
    /// An attachment is still uploading or cannot be sent.
    pub attachments_blocked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ResolvedAnswer {
    Text { value: String },
    Choices { values: Vec<String> },
}
impl ResolvedAnswer {
    /// An attachment-only answer is empty text.
    fn is_empty_text(&self) -> bool {
        matches!(self, Self::Text { value } if value.is_empty())
    }
    fn values(self) -> Vec<String> {
        match self {
            Self::Text { value } => vec![value],
            Self::Choices { values } => values,
        }
    }
}

/// The value an option answers with.
pub fn option_value(label: &str) -> String {
    js_trim(label).into()
}

fn resolve_option_value(question: &Question, value: &str) -> Option<String> {
    let label = js_trim(value);
    (!label.is_empty()
        && question
            .options
            .iter()
            .any(|option| js_trim(&option.label) == label))
    .then(|| label.to_owned())
}

fn selected_values(question: &Question, draft: Option<&QuestionDraft>) -> Vec<String> {
    let mut values: Vec<String> = vec![];
    for value in draft
        .map(|draft| draft.selected_option_values.as_slice())
        .unwrap_or_default()
    {
        if let Some(value) = resolve_option_value(question, value)
            && !values.contains(&value)
        {
            values.push(value);
        }
    }
    values
}

fn custom_answer(draft: Option<&QuestionDraft>) -> Option<String> {
    draft
        .map(|draft| js_trim(&draft.custom_answer))
        .filter(|answer| !answer.is_empty())
        .map(str::to_owned)
}

/// The answer a draft gives, or `None` while the question is unanswered.
pub fn resolve_question_answer(
    question: &Question,
    draft: Option<&QuestionDraft>,
) -> Option<ResolvedAnswer> {
    if draft.is_some_and(|draft| draft.attachments_blocked) {
        return None;
    }
    if let Some(value) = custom_answer(draft) {
        return Some(ResolvedAnswer::Text { value });
    }
    let selected = selected_values(question, draft);
    let attachments = draft.is_some_and(|draft| draft.attachment_count > 0);
    if question.multiple {
        if !selected.is_empty() {
            return Some(ResolvedAnswer::Choices { values: selected });
        }
    } else if let Some(value) = selected.into_iter().next() {
        return Some(ResolvedAnswer::Text { value });
    }
    attachments.then(|| ResolvedAnswer::Text {
        value: String::new(),
    })
}

fn draft_for<'a>(drafts: &'a [QuestionDraft], question: &Question) -> Option<&'a QuestionDraft> {
    drafts.iter().find(|draft| draft.question_id == question.id)
}

/// Typing an answer clears the chosen options.
pub fn set_question_custom_answer(
    question: &Question,
    draft: Option<&QuestionDraft>,
    custom_answer: &str,
) -> QuestionDraft {
    let selected_option_values = if js_trim(custom_answer).is_empty() {
        selected_values(question, draft)
    } else {
        vec![]
    };
    QuestionDraft {
        question_id: question.id.clone(),
        selected_option_values,
        custom_answer: custom_answer.into(),
        ..draft.cloned().unwrap_or_default()
    }
}

/// Choosing an option clears the typed answer; a multiple-choice question
/// toggles it.
pub fn toggle_question_option(
    question: &Question,
    draft: Option<&QuestionDraft>,
    option_value: &str,
) -> QuestionDraft {
    let base = || QuestionDraft {
        question_id: question.id.clone(),
        ..draft.cloned().unwrap_or_default()
    };
    let Some(value) = resolve_option_value(question, option_value) else {
        return base();
    };
    let selected_option_values = if question.multiple {
        let mut selected = selected_values(question, draft);
        if let Some(index) = selected.iter().position(|entry| entry == &value) {
            selected.remove(index);
        } else {
            selected.push(value);
        }
        selected
    } else {
        vec![value]
    };
    QuestionDraft {
        selected_option_values,
        custom_answer: String::new(),
        ..base()
    }
}

pub fn is_question_option_selected(
    question: &Question,
    draft: Option<&QuestionDraft>,
    option_value: &str,
) -> bool {
    if custom_answer(draft).is_some() {
        return false;
    }
    resolve_option_value(question, option_value)
        .is_some_and(|value| selected_values(question, draft).contains(&value))
}

/// Text typed as an answer moves back to the thread draft when an option
/// replaces it.
pub fn carry_displaced_custom_answer_into_prompt(prompt: &str, custom_answer: &str) -> String {
    let displaced = js_trim(custom_answer);
    if displaced.is_empty() {
        return prompt.into();
    }
    if js_trim(prompt).is_empty() {
        return displaced.into();
    }
    format!("{}\n\n{displaced}", prompt.trim_end_matches(is_js_space))
}

/// Every question's answer, or `None` while one is unanswered.
pub fn build_question_answers(
    questions: &[Question],
    drafts: &[QuestionDraft],
) -> Option<Vec<QuestionAnswer>> {
    questions
        .iter()
        .map(|question| {
            Some(QuestionAnswer {
                question_id: question.id.clone(),
                values: resolve_question_answer(question, draft_for(drafts, question))?.values(),
            })
        })
        .collect()
}

/// The answers a reply carries: typed and attachment-only answers are text,
/// chosen options of a multiple-choice question are choices.
pub fn question_answers(
    questions: &[Question],
    drafts: &[QuestionDraft],
) -> Option<agent_domain::Answers> {
    questions
        .iter()
        .map(|question| {
            let answer = match resolve_question_answer(question, draft_for(drafts, question))? {
                ResolvedAnswer::Text { value } => agent_domain::Answer::Text(value),
                ResolvedAnswer::Choices { values } => agent_domain::Answer::Choices(values),
            };
            Some((question.id.clone(), answer))
        })
        .collect()
}

pub fn count_answered_questions(questions: &[Question], drafts: &[QuestionDraft]) -> usize {
    questions
        .iter()
        .filter(|question| resolve_question_answer(question, draft_for(drafts, question)).is_some())
        .count()
}

/// The first question without an answer (an attachment-only answer counts as
/// none), else the last question.
pub fn first_unanswered_question_index(questions: &[Question], drafts: &[QuestionDraft]) -> usize {
    questions
        .iter()
        .position(|question| {
            resolve_question_answer(question, draft_for(drafts, question))
                .is_none_or(|answer| answer.is_empty_text())
        })
        .unwrap_or(questions.len().saturating_sub(1))
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct QuestionProgress {
    pub question_index: u32,
    pub active_question_id: Option<String>,
    pub selected_option_values: Vec<String>,
    pub custom_answer: String,
    pub resolved_answer: Option<ResolvedAnswer>,
    pub using_custom_answer: bool,
    pub answered_question_count: u32,
    pub is_last_question: bool,
    pub is_complete: bool,
    pub can_advance: bool,
}

pub fn question_progress(
    questions: &[Question],
    drafts: &[QuestionDraft],
    question_index: usize,
) -> QuestionProgress {
    let index = question_index.min(questions.len().saturating_sub(1));
    let active = questions.get(index);
    let draft = active.and_then(|question| draft_for(drafts, question));
    let resolved = active.and_then(|question| resolve_question_answer(question, draft));
    let custom_answer = draft.map_or_else(String::new, |draft| draft.custom_answer.clone());
    QuestionProgress {
        question_index: index as u32,
        active_question_id: active.map(|question| question.id.clone()),
        selected_option_values: active
            .map(|question| selected_values(question, draft))
            .unwrap_or_default(),
        using_custom_answer: !js_trim(&custom_answer).is_empty(),
        custom_answer,
        can_advance: resolved.is_some(),
        resolved_answer: resolved,
        answered_question_count: count_answered_questions(questions, drafts) as u32,
        is_last_question: questions.is_empty() || index + 1 >= questions.len(),
        is_complete: build_question_answers(questions, drafts).is_some(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct QuestionOptionView {
    pub label: String,
    pub value: String,
    /// Shown when it says more than the label.
    pub description: Option<String>,
    pub selected: bool,
    /// The 1-9 key that picks it on desktop.
    pub shortcut: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct QuestionView {
    pub id: String,
    pub header: String,
    pub question: String,
    pub multiple: bool,
    pub hint: Option<String>,
    pub custom_answer: String,
    pub options: Vec<QuestionOptionView>,
}

fn question_view(question: &Question, draft: Option<&QuestionDraft>) -> QuestionView {
    QuestionView {
        id: question.id.clone(),
        header: question.header.clone(),
        question: question.question.clone(),
        multiple: question.multiple,
        hint: question.multiple.then(|| MULTIPLE_CHOICE_HINT.into()),
        custom_answer: draft.map_or_else(String::new, |draft| draft.custom_answer.clone()),
        options: question
            .options
            .iter()
            .enumerate()
            .map(|(index, option)| {
                let value = option_value(&option.label);
                QuestionOptionView {
                    label: option.label.clone(),
                    selected: is_question_option_selected(question, draft, &value),
                    value,
                    description: option
                        .description
                        .clone()
                        .filter(|description| description != &option.label),
                    shortcut: (index < 9).then_some(index as u32 + 1),
                }
            })
            .collect(),
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct QuestionsView {
    pub request_id: String,
    pub dismissible: bool,
    /// Message questions stay answerable after the provider turn ends.
    pub can_respond: bool,
    /// A reply or dismissal from this device is waiting for the Host.
    pub responding: bool,
    pub question_count: u32,
    /// "i/N" while the request asks more than one question.
    pub counter: Option<String>,
    pub count_label: String,
    pub expand_accessibility_label: String,
    pub progress: QuestionProgress,
    pub active: Option<QuestionView>,
    pub questions: Vec<QuestionView>,
    /// What `Intent::RespondQuestions` sends, once every question is answered.
    pub answers: Option<Vec<QuestionAnswer>>,
    pub submit_enabled: bool,
    pub unavailable_notice: Option<String>,
}

pub fn questions_view(
    request: &Request,
    drafts: &[QuestionDraft],
    question_index: usize,
    responding: bool,
) -> QuestionsView {
    let questions = questions_of(request);
    let progress = question_progress(questions, drafts, question_index);
    let can_respond = request.capability != ResponseCapability::NotResumable;
    let answers = build_question_answers(questions, drafts);
    let count = questions.len();
    let questions_label = quantity(count, "question");
    QuestionsView {
        request_id: request.id.to_string(),
        dismissible: question_dismissible(request),
        can_respond,
        responding,
        question_count: count as u32,
        counter: (count > 1).then(|| format!("{}/{count}", progress.question_index + 1)),
        count_label: questions_label.clone(),
        expand_accessibility_label: format!("Expand user input, {questions_label}"),
        active: questions
            .get(progress.question_index as usize)
            .map(|question| question_view(question, draft_for(drafts, question))),
        questions: questions
            .iter()
            .map(|question| question_view(question, draft_for(drafts, question)))
            .collect(),
        progress,
        submit_enabled: can_respond && !responding && answers.is_some(),
        answers,
        unavailable_notice: (!can_respond).then(|| REQUEST_UNAVAILABLE_NOTICE.into()),
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct RequestsView {
    /// The oldest pending approval.
    pub approval: Option<ApprovalView>,
    pub approval_count: u32,
    /// The oldest pending question request.
    pub questions: Option<QuestionsView>,
    pub question_request_count: u32,
}

/// Requests this device answered or dismissed that the Host has not confirmed.
pub fn responding_requests(state: &State, outbox: &Outbox) -> Vec<RuntimeRequestId> {
    outbox_commands(state, outbox)
        .filter_map(|command| match command {
            Command::Respond { request, .. } | Command::DismissQuestion { request } => {
                Some(request.clone())
            }
            _ => None,
        })
        .collect()
}

/// `drafts` and `question_index` are the device's answers to the oldest
/// question request.
pub fn requests_view(
    state: &State,
    outbox: &Outbox,
    drafts: &[QuestionDraft],
    question_index: usize,
) -> RequestsView {
    let responding = responding_requests(state, outbox);
    let approvals = pending_approvals(state);
    let questions = pending_questions(state);
    RequestsView {
        approval: approvals.first().and_then(|request| {
            approval_view(request, approvals.len(), responding.contains(&request.id))
        }),
        approval_count: approvals.len() as u32,
        questions: questions.first().map(|request| {
            questions_view(
                request,
                drafts,
                question_index,
                responding.contains(&request.id),
            )
        }),
        question_request_count: questions.len() as u32,
    }
}

/// A question request's answer drafts with the state of each answer's files.
pub fn answer_drafts(snapshot: &Snapshot, request_id: &str) -> Vec<QuestionDraft> {
    let mut drafts = snapshot
        .question_drafts
        .get(request_id)
        .map(|drafts| drafts.drafts.clone())
        .unwrap_or_default();
    let prefix = answer_draft_key(request_id, "");
    for (key, files) in snapshot.drafts.iter() {
        let Some(question) = key
            .strip_prefix(&prefix)
            .filter(|_| !files.attachments.is_empty())
        else {
            continue;
        };
        let attachment_count = files.attachments.len() as u32;
        let attachments_blocked = files.attachments.iter().any(|a| a.status != "ready");
        match drafts
            .iter_mut()
            .find(|draft| draft.question_id == question)
        {
            Some(draft) => {
                draft.attachment_count = attachment_count;
                draft.attachments_blocked = attachments_blocked;
            }
            None => drafts.push(QuestionDraft {
                question_id: question.into(),
                attachment_count,
                attachments_blocked,
                ..Default::default()
            }),
        }
    }
    drafts
}

/// A thread's requests with this device's answers to its oldest question request.
pub fn thread_requests_view(snapshot: &Snapshot, state: &State) -> RequestsView {
    let request = pending_questions(state)
        .first()
        .map(|request| request.id.to_string());
    let (drafts, question_index) = request.map_or_else(Default::default, |request| {
        (
            answer_drafts(snapshot, &request),
            snapshot
                .question_drafts
                .get(&request)
                .map_or(0, |drafts| drafts.question_index as usize),
        )
    });
    requests_view(state, &snapshot.outbox, &drafts, question_index)
}

/// The tallest the mobile question card may grow above the keyboard and composer.
pub fn pending_input_max_height(
    window_height: f64,
    keyboard_height: f64,
    navigation_header_height: f64,
    composer_overlap_height: f64,
) -> f64 {
    let available = window_height
        - keyboard_height.max(0.0)
        - navigation_header_height.max(0.0)
        - composer_overlap_height.max(0.0)
        - PENDING_USER_INPUT_VERTICAL_GAP;
    available.clamp(PENDING_USER_INPUT_MIN_HEIGHT, PENDING_USER_INPUT_MAX_HEIGHT)
}

#[cfg(test)]
mod tests;
