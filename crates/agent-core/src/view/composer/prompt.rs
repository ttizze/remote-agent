//! The composer editor: its placeholder, whether it accepts input, whether the
//! draft has something to send and whether the prompt fits a provider turn.
use super::actions::SessionPhase;
use crate::js_text::utf16_len;

/// The longest provider turn input, in UTF-16 code units.
pub const PROVIDER_SEND_TURN_MAX_INPUT_CHARS: usize = 120_000;

pub const DEFAULT_COMPOSER_PLACEHOLDER: &str =
    "Ask anything, @tag files/folders, $use skills, or / for commands";
pub const DISCONNECTED_COMPOSER_PLACEHOLDER: &str =
    "Ask for changes, send follow-ups, or attach images";
pub const MOBILE_THREAD_COMPOSER_PLACEHOLDER: &str = "Ask the repo agent, or run a command…";
pub const MOBILE_NEW_TASK_COMPOSER_PLACEHOLDER: &str = "Ask anything…";

/// What the composer is waiting on, in the order the placeholder considers it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EditorState {
    pub pending_approval: bool,
    /// The question being answered; `Some(true)` when only its options count.
    pub pending_question_choice_only: Option<bool>,
    pub pending_answer_responding: bool,
    pub plan_follow_up_with_plan: bool,
    pub project_selection_required: bool,
    pub provider_unavailable: bool,
    pub is_connecting: bool,
    pub phase: Option<SessionPhase>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ComposerEditor {
    pub placeholder: String,
    pub disabled: bool,
}

pub fn composer_editor(state: &EditorState) -> ComposerEditor {
    let placeholder = if state.pending_approval {
        "Resolve this approval request to continue"
    } else if let Some(choice_only) = state.pending_question_choice_only {
        if choice_only {
            "Choose an option above"
        } else {
            "Type your own answer, or leave this blank to use the selected option"
        }
    } else if state.plan_follow_up_with_plan {
        "Add feedback to refine the plan, or leave this blank to implement it"
    } else if state.project_selection_required {
        "Choose a project above to start a thread"
    } else if state.provider_unavailable {
        "Enable a provider in Settings to send a message"
    } else if state.phase == Some(SessionPhase::Disconnected) {
        DISCONNECTED_COMPOSER_PLACEHOLDER
    } else {
        DEFAULT_COMPOSER_PLACEHOLDER
    };
    ComposerEditor {
        placeholder: placeholder.into(),
        disabled: state.is_connecting
            || state.pending_approval
            || state.project_selection_required
            || state.pending_question_choice_only == Some(true)
            || state.pending_answer_responding,
    }
}

/// Removes `[label](context://v1/…)` links, which stand for context records
/// rather than typed text.
pub fn strip_inline_context_references(prompt: &str) -> String {
    let mut text = String::with_capacity(prompt.len());
    let mut cursor = 0;
    for reference in agent_domain::context_references(prompt) {
        text.push_str(&prompt[cursor..reference.start]);
        cursor = reference.end;
    }
    text.push_str(&prompt[cursor..]);
    text
}

/// A draft is sendable with typed text, an attachment or a context record.
pub fn has_sendable_content(prompt: &str, attachment_count: usize, context_count: usize) -> bool {
    !strip_inline_context_references(prompt).trim().is_empty()
        || attachment_count > 0
        || context_count > 0
}

/// The provider turn contract trims the input before measuring it.
pub fn prompt_length_validation_message(prompt: &str) -> Option<String> {
    let excess = utf16_len(prompt.trim()).checked_sub(PROVIDER_SEND_TURN_MAX_INPUT_CHARS)?;
    (excess > 0).then(|| {
        format!(
            "Prompt is {} {} over the {}-character limit. Shorten or split it before sending.",
            super::group_thousands(excess),
            if excess == 1 {
                "character"
            } else {
                "characters"
            },
            super::group_thousands(PROVIDER_SEND_TURN_MAX_INPUT_CHARS)
        )
    })
}

/// Where a composer submission goes; answers to questions take their own path
/// without the turn limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionTarget {
    ProviderTurn,
    PendingUserInput,
}

/// `provider_input` is the composed turn input (with appended context or a
/// generated plan follow-up) when it differs from the draft.
pub fn submission_validation_message(
    prompt: &str,
    provider_input: Option<&str>,
    target: SubmissionTarget,
) -> Option<String> {
    match target {
        SubmissionTarget::ProviderTurn => {
            prompt_length_validation_message(provider_input.unwrap_or(prompt))
        }
        SubmissionTarget::PendingUserInput => None,
    }
}

/// Mobile holds a send while a queued edit saves, the task is still being
/// created, pasted text is turning into an attachment or an attachment blocks it.
pub fn mobile_send_blocked_reason(
    saving_queued_edit: bool,
    preparing_task: bool,
    pending_pasted_text: bool,
    attachment_block_reason: Option<String>,
) -> Option<String> {
    if saving_queued_edit {
        Some("Saving…".into())
    } else if preparing_task {
        Some("Starting the task…".into())
    } else if pending_pasted_text {
        Some("Attaching pasted text".into())
    } else {
        attachment_block_reason
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limit_message(prompt: &str, provider_input: Option<&str>) -> Option<String> {
        submission_validation_message(prompt, provider_input, SubmissionTarget::ProviderTurn)
    }

    #[test]
    fn keeps_an_oversized_draft_editable_and_sends_a_corrected_follow_up() {
        let draft = "x".repeat(PROVIDER_SEND_TURN_MAX_INPUT_CHARS + 1);
        assert_eq!(
            limit_message(&draft, None).as_deref(),
            Some(
                "Prompt is 1 character over the 120,000-character limit. Shorten or split it before sending."
            )
        );
        assert_eq!(limit_message("Corrected prompt", None), None);
    }

    #[test]
    fn allows_a_draft_at_the_shared_character_limit_through_the_normal_send_path() {
        assert_eq!(
            limit_message(&"x".repeat(PROVIDER_SEND_TURN_MAX_INPUT_CHARS), None),
            None
        );
    }

    #[test]
    fn blocks_when_appended_context_pushes_the_provider_input_over_the_shared_limit() {
        let draft = "x".repeat(PROVIDER_SEND_TURN_MAX_INPUT_CHARS);
        assert_eq!(
            limit_message(&draft, Some(&format!("{draft}\n\nTerminal context"))).as_deref(),
            Some(
                "Prompt is 18 characters over the 120,000-character limit. Shorten or split it before sending."
            )
        );
        assert_eq!(
            limit_message(
                "Corrected prompt",
                Some("Corrected prompt\n\nShort terminal context")
            ),
            None
        );
    }

    #[test]
    fn allows_fully_composed_provider_input_at_the_shared_character_limit() {
        assert_eq!(
            limit_message(
                "Short draft",
                Some(&"x".repeat(PROVIDER_SEND_TURN_MAX_INPUT_CHARS))
            ),
            None
        );
    }

    #[test]
    fn blocks_a_generated_plan_follow_up_that_exceeds_the_shared_limit() {
        let input = format!(
            "PLEASE IMPLEMENT THIS PLAN:\n{}",
            "x".repeat(PROVIDER_SEND_TURN_MAX_INPUT_CHARS)
        );
        assert!(
            limit_message("", Some(&input))
                .unwrap()
                .contains("over the 120,000-character limit")
        );
    }

    #[test]
    fn allows_surrounding_whitespace_that_the_provider_turn_contract_trims() {
        let draft = format!(" {} ", "x".repeat(PROVIDER_SEND_TURN_MAX_INPUT_CHARS));
        assert_eq!(limit_message(&draft, None), None);
    }

    #[test]
    fn dispatches_pending_user_input_answers_on_their_separate_response_path() {
        let answer = "x".repeat(PROVIDER_SEND_TURN_MAX_INPUT_CHARS + 1);
        assert_eq!(
            submission_validation_message(&answer, None, SubmissionTarget::PendingUserInput),
            None
        );
    }

    #[test]
    fn counts_utf16_units_and_groups_large_excess() {
        let draft = "😀".repeat(PROVIDER_SEND_TURN_MAX_INPUT_CHARS / 2 + 1_000);
        assert_eq!(
            prompt_length_validation_message(&draft).as_deref(),
            Some(
                "Prompt is 2,000 characters over the 120,000-character limit. Shorten or split it before sending."
            )
        );
    }

    #[test]
    fn the_placeholder_follows_what_the_composer_waits_on() {
        let placeholder = |state: EditorState| composer_editor(&state);
        assert_eq!(
            placeholder(EditorState {
                pending_approval: true,
                pending_question_choice_only: Some(false),
                ..Default::default()
            }),
            ComposerEditor {
                placeholder: "Resolve this approval request to continue".into(),
                disabled: true
            }
        );
        assert_eq!(
            placeholder(EditorState {
                pending_question_choice_only: Some(true),
                plan_follow_up_with_plan: true,
                ..Default::default()
            }),
            ComposerEditor {
                placeholder: "Choose an option above".into(),
                disabled: true
            }
        );
        assert_eq!(
            placeholder(EditorState {
                pending_question_choice_only: Some(false),
                ..Default::default()
            })
            .placeholder,
            "Type your own answer, or leave this blank to use the selected option"
        );
        assert_eq!(
            placeholder(EditorState {
                plan_follow_up_with_plan: true,
                project_selection_required: true,
                ..Default::default()
            })
            .placeholder,
            "Add feedback to refine the plan, or leave this blank to implement it"
        );
        assert_eq!(
            placeholder(EditorState {
                project_selection_required: true,
                ..Default::default()
            }),
            ComposerEditor {
                placeholder: "Choose a project above to start a thread".into(),
                disabled: true
            }
        );
        assert_eq!(
            placeholder(EditorState {
                provider_unavailable: true,
                phase: Some(SessionPhase::Disconnected),
                ..Default::default()
            })
            .placeholder,
            "Enable a provider in Settings to send a message"
        );
        assert_eq!(
            placeholder(EditorState {
                phase: Some(SessionPhase::Disconnected),
                ..Default::default()
            })
            .placeholder,
            DISCONNECTED_COMPOSER_PLACEHOLDER
        );
        assert_eq!(
            placeholder(EditorState {
                phase: Some(SessionPhase::Ready),
                is_connecting: true,
                ..Default::default()
            }),
            ComposerEditor {
                placeholder: DEFAULT_COMPOSER_PLACEHOLDER.into(),
                disabled: true
            }
        );
        assert!(
            placeholder(EditorState {
                pending_question_choice_only: Some(false),
                pending_answer_responding: true,
                ..Default::default()
            })
            .disabled
        );
    }

    #[test]
    fn context_links_alone_are_not_typed_content() {
        let link = "[Image: shot.png](context://v1/image/ctx_1)";
        assert!(!has_sendable_content(&format!(" {link} \n"), 0, 0));
        assert!(has_sendable_content(link, 0, 1));
        assert!(has_sendable_content("", 1, 0));
        assert!(has_sendable_content(&format!("Look {link}"), 0, 0));
        assert_eq!(
            strip_inline_context_references(&format!("a {link} b")),
            "a  b"
        );
    }

    #[test]
    fn mobile_holds_a_send_for_the_first_reason_that_applies() {
        assert_eq!(
            mobile_send_blocked_reason(true, true, true, Some("Uploading".into())).as_deref(),
            Some("Saving…")
        );
        assert_eq!(
            mobile_send_blocked_reason(false, true, true, None).as_deref(),
            Some("Starting the task…")
        );
        assert_eq!(
            mobile_send_blocked_reason(false, false, true, None).as_deref(),
            Some("Attaching pasted text")
        );
        assert_eq!(
            mobile_send_blocked_reason(false, false, false, Some("Uploading".into())).as_deref(),
            Some("Uploading")
        );
    }
}
