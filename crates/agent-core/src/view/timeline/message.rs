//! Decorations around user and assistant messages: attribution, intent
//! markers, status chips, collapsing, copy text and per-message actions.
use crate::js_text::{is_js_space, utf16_len};
use agent_domain::{
    InputIntent, Item, ItemKind, ItemStatus, Message, MessageAuthor, RunId, State, ThreadId,
    context_references,
};

fn status_name(status: ItemStatus) -> &'static str {
    match status {
        ItemStatus::Pending => "pending",
        ItemStatus::Running => "running",
        ItemStatus::Waiting => "waiting",
        ItemStatus::Completed => "completed",
        ItemStatus::Interrupted => "interrupted",
        ItemStatus::Failed => "failed",
        ItemStatus::Cancelled => "cancelled",
    }
}

/// A user-role message written by an agent rather than the user.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AgentAttribution {
    pub label: String,
    /// The thread that sent it, when known; the label then opens it.
    pub sender_thread: Option<ThreadId>,
    pub open_label: String,
}

/// A delegated thread knows its parent sent the delegated prompt.
pub fn user_message_attribution(state: &State, message: &Message) -> Option<AgentAttribution> {
    (message.created_by == MessageAuthor::Agent).then(|| AgentAttribution {
        label: "Sent by another agent".into(),
        sender_thread: state
            .delegation
            .as_ref()
            .filter(|delegation| delegation.message == message.id)
            .map(|delegation| delegation.parent.clone()),
        open_label: "Open sending thread".into(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum IntentTone {
    Queued,
    Steer,
}

/// The desktop marker above a user message that did not start a turn.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct IntentMarker {
    pub label: String,
    pub tooltip: String,
    pub tone: IntentTone,
    /// Steering draws a redo arrow before the label.
    pub steer_icon: bool,
}

fn intent_tooltip(intent: InputIntent) -> &'static str {
    match intent {
        InputIntent::QueuedTurn => "Queued behind the active turn",
        InputIntent::PromotedQueuedToSteer => {
            "Originally queued, then promoted to steer the active turn"
        }
        InputIntent::TurnStart | InputIntent::Steer => "Steered the active turn",
    }
}

pub fn user_message_intent_marker(intent: InputIntent) -> Option<IntentMarker> {
    let queued = match intent {
        InputIntent::TurnStart => return None,
        InputIntent::QueuedTurn => true,
        InputIntent::Steer | InputIntent::PromotedQueuedToSteer => false,
    };
    Some(IntentMarker {
        label: if queued { "Queued" } else { "Steer" }.into(),
        tooltip: intent_tooltip(intent).into(),
        tone: if queued {
            IntentTone::Queued
        } else {
            IntentTone::Steer
        },
        steer_icon: !queued,
    })
}

/// The mobile badge beside a user message that did not start a turn.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct IntentBadge {
    pub label: String,
    pub accessibility_label: String,
    pub tone: IntentTone,
}

pub fn user_message_intent_badge(intent: InputIntent) -> Option<IntentBadge> {
    let (label, tone) = match intent {
        InputIntent::TurnStart => return None,
        InputIntent::QueuedTurn => ("queued", IntentTone::Queued),
        InputIntent::Steer => ("steer", IntentTone::Steer),
        InputIntent::PromotedQueuedToSteer => ("queued → steer", IntentTone::Steer),
    };
    Some(IntentBadge {
        label: label.into(),
        accessibility_label: intent_tooltip(intent).into(),
        tone,
    })
}

const MAX_COLLAPSED_USER_MESSAGE_LINES: usize = 8;
const MAX_COLLAPSED_USER_MESSAGE_LENGTH: usize = 600;
pub const SHOW_FULL_MESSAGE: &str = "Show full message";
pub const SHOW_LESS: &str = "Show less";

/// Long bodies start clipped behind "Show full message".
pub fn user_message_collapsible(text: &str) -> bool {
    !text.trim_matches(is_js_space).is_empty()
        && (utf16_len(text) > MAX_COLLAPSED_USER_MESSAGE_LENGTH
            || text.split('\n').count() > MAX_COLLAPSED_USER_MESSAGE_LINES)
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct UserMessageCopy {
    pub text: String,
    /// The message carries context records; clients add the structured
    /// clipboard fragment so a paste brings their payloads along.
    pub with_context: bool,
}

/// Context links stay canonical when the records travel along; otherwise
/// they copy as their labels.
pub fn user_message_copy(message: &Message) -> Option<UserMessageCopy> {
    if message.text.is_empty() {
        return None;
    }
    let with_context = message
        .context
        .as_ref()
        .is_some_and(|context| !context.records.is_empty());
    if with_context {
        return Some(UserMessageCopy {
            text: message.text.clone(),
            with_context,
        });
    }
    let mut text = String::with_capacity(message.text.len());
    let mut cursor = 0;
    for reference in context_references(&message.text) {
        text.push_str(&message.text[cursor..reference.start]);
        text.push_str(&reference.label);
        cursor = reference.end;
    }
    text.push_str(&message.text[cursor..]);
    Some(UserMessageCopy { text, with_context })
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct EditFromHere {
    pub turn_count: u64,
    pub enabled: bool,
    pub label: String,
}

/// `revert_turn_count` comes from the row derivation for messages that can
/// be rolled back to.
pub fn edit_from_here(
    revert_turn_count: Option<u64>,
    reverting: bool,
    working: bool,
) -> Option<EditFromHere> {
    revert_turn_count.map(|turn_count| EditFromHere {
        turn_count,
        enabled: !reverting && !working,
        label: "Edit from here".into(),
    })
}

/// A user item that did not settle normally shows its status in red.
pub fn user_status_chip(item: Option<&Item>) -> Option<String> {
    item.map(|item| item.status)
        .filter(|status| {
            !matches!(
                status,
                ItemStatus::Completed | ItemStatus::Pending | ItemStatus::Waiting
            )
        })
        .map(|status| status_name(status).into())
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct UserMessageDecorations {
    pub attribution: Option<AgentAttribution>,
    pub intent: Option<IntentMarker>,
    pub collapsible: bool,
    pub status_chip: Option<String>,
    pub copy: Option<UserMessageCopy>,
    pub edit_from_here: Option<EditFromHere>,
}

pub fn user_message_decorations(
    state: &State,
    item: Option<&Item>,
    message: &Message,
    revert_turn_count: Option<u64>,
    reverting: bool,
    working: bool,
) -> UserMessageDecorations {
    UserMessageDecorations {
        attribution: user_message_attribution(state, message),
        intent: user_message_intent_marker(message.intent),
        collapsible: user_message_collapsible(&message.text),
        status_chip: user_status_chip(item),
        copy: user_message_copy(message),
        edit_from_here: edit_from_here(revert_turn_count, reverting, working),
    }
}

/// An empty settled response still says so.
pub fn assistant_display_text(message: &Message) -> String {
    if message.text.is_empty() && !message.streaming {
        "(empty response)".into()
    } else {
        message.text.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ForkAction {
    pub run: RunId,
    pub label: String,
}

/// A completed response of a run can start a fork. Without provider
/// capability records the Host's portable fork always applies.
pub fn assistant_fork(item: &Item) -> Option<ForkAction> {
    match (&item.kind, &item.run, item.status) {
        (ItemKind::AssistantMessage { .. }, Some(run), ItemStatus::Completed) => Some(ForkAction {
            run: run.clone(),
            label: "Fork from this response".into(),
        }),
        _ => None,
    }
}

/// The mobile client names a fork after its source thread.
pub fn fork_thread_title(source_title: &str) -> String {
    format!("{source_title} fork")
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AssistantCopyState {
    pub text: Option<String>,
    pub visible: bool,
}

/// Copy appears on a settled response with text.
pub fn assistant_copy_state(
    text: Option<&str>,
    show_copy_button: bool,
    streaming: bool,
) -> AssistantCopyState {
    let text = text.filter(|text| !text.trim_matches(is_js_space).is_empty());
    let visible = show_copy_button && text.is_some() && !streaming;
    AssistantCopyState {
        visible,
        text: text.map(|text| {
            if visible {
                crate::presentation::markdown::directives::render_directives_for_copy(text)
            } else {
                text.to_owned()
            }
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AssistantMeta {
    pub fork: Option<ForkAction>,
    pub status_chip: Option<String>,
    pub copy: AssistantCopyState,
    pub show_timestamp: bool,
}

/// The row under the last response of a settled turn.
pub fn assistant_meta(
    item: Option<&Item>,
    message: &Message,
    show_copy_button: bool,
    copy_streaming: bool,
) -> AssistantMeta {
    AssistantMeta {
        fork: item.and_then(assistant_fork),
        status_chip: item
            .map(|item| item.status)
            .filter(|status| *status != ItemStatus::Completed)
            .map(|status| status_name(status).into()),
        copy: assistant_copy_state(Some(&message.text), show_copy_button, copy_streaming),
        show_timestamp: !message.streaming,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_domain::{
        Delegation, Json, MessageContext, MessageId, NodeId, Role, Timestamp, TurnItemId,
    };

    fn message(text: &str) -> Message {
        let at = Timestamp::parse("2026-03-17T19:12:28.000Z").unwrap();
        Message {
            notification: None,
            id: MessageId::new("message-1").unwrap(),
            run: Some(RunId::new("run-1").unwrap()),
            role: Role::User,
            text: text.into(),
            attachments: vec![],
            intent: InputIntent::TurnStart,
            streaming: false,
            created_by: MessageAuthor::User,
            creation_source: "desktop".into(),
            created_at: at.clone(),
            updated_at: at,
            context: None,
        }
    }
    fn item(kind: ItemKind, status: ItemStatus) -> Item {
        Item {
            id: TurnItemId::new("item-1").unwrap(),
            run: Some(RunId::new("run-1").unwrap()),
            attempt: None,
            native_key: String::new(),
            ordinal: 0,
            kind,
            status,
            text: "Done".into(),
            started_at: Timestamp::parse("2026-03-17T19:12:28.000Z").unwrap(),
            completed_at: None,
            output_omitted: false,
            output_indicates_failure: false,
        }
    }
    fn assistant_item(status: ItemStatus) -> Item {
        item(
            ItemKind::AssistantMessage {
                message: MessageId::new("assistant-message-1").unwrap(),
            },
            status,
        )
    }
    fn long_text() -> String {
        (1..=12)
            .map(|line| format!("Line {line}: a user prompt that keeps going."))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn does_not_label_ordinary_turn_start_messages() {
        assert_eq!(user_message_intent_badge(InputIntent::TurnStart), None);
    }

    #[test]
    fn labels_messages_waiting_behind_the_active_turn() {
        assert_eq!(
            user_message_intent_badge(InputIntent::QueuedTurn),
            Some(IntentBadge {
                label: "queued".into(),
                accessibility_label: "Queued behind the active turn".into(),
                tone: IntentTone::Queued,
            })
        );
    }

    #[test]
    fn labels_messages_that_steer_the_active_turn() {
        assert_eq!(
            user_message_intent_badge(InputIntent::Steer),
            Some(IntentBadge {
                label: "steer".into(),
                accessibility_label: "Steered the active turn".into(),
                tone: IntentTone::Steer,
            })
        );
    }

    #[test]
    fn preserves_the_queued_origin_after_promotion_to_steer() {
        assert_eq!(
            user_message_intent_badge(InputIntent::PromotedQueuedToSteer),
            Some(IntentBadge {
                label: "queued → steer".into(),
                accessibility_label: "Originally queued, then promoted to steer the active turn"
                    .into(),
                tone: IntentTone::Steer,
            })
        );
    }

    #[test]
    fn keeps_steer_intent_visible_on_committed_user_messages() {
        let marker = user_message_intent_marker(InputIntent::Steer).unwrap();
        assert_eq!(marker.label, "Steer");
        assert!(marker.steer_icon);
        assert_eq!(marker.tooltip, "Steered the active turn");
        let queued = user_message_intent_marker(InputIntent::QueuedTurn).unwrap();
        assert_eq!(
            (queued.label.as_str(), queued.steer_icon),
            ("Queued", false)
        );
        let promoted = user_message_intent_marker(InputIntent::PromotedQueuedToSteer).unwrap();
        assert_eq!(promoted.label, "Steer");
        assert_eq!(
            promoted.tooltip,
            "Originally queued, then promoted to steer the active turn"
        );
        assert_eq!(user_message_intent_marker(InputIntent::TurnStart), None);
    }

    #[test]
    fn renders_collapse_controls_for_long_user_messages() {
        assert!(user_message_collapsible(&long_text()));
        assert!(user_message_collapsible(&"é".repeat(601)));
        assert!(!user_message_collapsible(&"é".repeat(600)));
        assert!(user_message_collapsible(
            &"\n".repeat(8).replace('\n', "x\n")
        ));
        assert!(!user_message_collapsible(&"\n".repeat(20)));
    }

    #[test]
    fn does_not_render_collapse_controls_for_short_user_messages() {
        assert!(!user_message_collapsible("Short prompt."));
        assert!(!user_message_collapsible(&"x\n".repeat(7)));
    }

    #[test]
    fn keeps_the_copy_button_for_collapsed_long_user_messages() {
        let long = message(&long_text());
        let decorations =
            user_message_decorations(&State::default(), None, &long, None, false, false);
        assert!(decorations.collapsible);
        assert_eq!(decorations.copy.unwrap().text, long_text());
    }

    #[test]
    fn identifies_user_role_messages_sent_by_another_agent() {
        let mut agent = message("Review this area");
        agent.created_by = MessageAuthor::Agent;
        agent.creation_source = "provider".into();
        let state = State::default();
        let attribution = user_message_attribution(&state, &agent).unwrap();
        assert_eq!(attribution.label, "Sent by another agent");
        assert_eq!(attribution.sender_thread, None);
        assert_eq!(
            user_message_attribution(&state, &message("Review this area")),
            None
        );
        let delegated = State {
            delegation: Some(Delegation {
                parent: ThreadId::new("parent").unwrap(),
                task: NodeId::new("task").unwrap(),
                message: MessageId::new("message-1").unwrap(),
            }),
            ..State::default()
        };
        assert_eq!(
            user_message_attribution(&delegated, &agent)
                .unwrap()
                .sender_thread,
            Some(ThreadId::new("parent").unwrap())
        );
    }

    #[test]
    fn copies_context_links_as_labels_unless_their_records_travel() {
        let text = "Look at [shot.png](context://v1/image/ctx_1) now";
        assert_eq!(
            user_message_copy(&message(text)),
            Some(UserMessageCopy {
                text: "Look at shot.png now".into(),
                with_context: false,
            })
        );
        let mut with_records = message(text);
        with_records.context = Some(MessageContext {
            version: 1,
            records: vec![Json(serde_json::json!({ "contextId": "ctx_1" }))],
        });
        assert_eq!(
            user_message_copy(&with_records),
            Some(UserMessageCopy {
                text: text.into(),
                with_context: true,
            })
        );
        assert_eq!(user_message_copy(&message("")), None);
    }

    #[test]
    fn shows_a_status_chip_only_for_unsettled_user_items() {
        let user = |status| {
            item(
                ItemKind::UserMessage {
                    message: MessageId::new("message-1").unwrap(),
                },
                status,
            )
        };
        assert_eq!(
            user_status_chip(Some(&user(ItemStatus::Failed))).as_deref(),
            Some("failed")
        );
        assert_eq!(
            user_status_chip(Some(&user(ItemStatus::Interrupted))).as_deref(),
            Some("interrupted")
        );
        for status in [
            ItemStatus::Completed,
            ItemStatus::Pending,
            ItemStatus::Waiting,
        ] {
            assert_eq!(user_status_chip(Some(&user(status))), None);
        }
        assert_eq!(user_status_chip(None), None);
    }

    #[test]
    fn disables_edit_from_here_while_reverting_or_working() {
        assert_eq!(edit_from_here(None, false, false), None);
        let edit = edit_from_here(Some(2), false, false).unwrap();
        assert_eq!((edit.turn_count, edit.enabled), (2, true));
        assert_eq!(edit.label, "Edit from here");
        assert!(!edit_from_here(Some(2), true, false).unwrap().enabled);
        assert!(!edit_from_here(Some(2), false, true).unwrap().enabled);
    }

    #[test]
    fn exposes_a_per_response_fork_action_for_completed_assistant_items() {
        let mut done = message("Done");
        done.role = Role::Assistant;
        let meta = assistant_meta(
            Some(&assistant_item(ItemStatus::Completed)),
            &done,
            true,
            false,
        );
        assert_eq!(
            meta.fork,
            Some(ForkAction {
                run: RunId::new("run-1").unwrap(),
                label: "Fork from this response".into(),
            })
        );
        assert_eq!(meta.status_chip, None);
        assert!(meta.copy.visible && meta.show_timestamp);
    }

    #[test]
    fn allows_native_portable_and_capability_unknown_exact_run_forks() {
        assert!(assistant_fork(&assistant_item(ItemStatus::Completed)).is_some());
        assert_eq!(assistant_fork(&assistant_item(ItemStatus::Running)), None);
        let mut runless = assistant_item(ItemStatus::Completed);
        runless.run = None;
        assert_eq!(assistant_fork(&runless), None);
        let user = item(
            ItemKind::UserMessage {
                message: MessageId::new("message-1").unwrap(),
            },
            ItemStatus::Completed,
        );
        assert_eq!(assistant_fork(&user), None);
        let meta = assistant_meta(
            Some(&assistant_item(ItemStatus::Interrupted)),
            &message("Partial"),
            true,
            false,
        );
        assert_eq!(
            (meta.fork, meta.status_chip.as_deref()),
            (None, Some("interrupted"))
        );
        assert_eq!(fork_thread_title("Parser work"), "Parser work fork");
    }

    #[test]
    fn returns_enabled_copy_state_for_completed_assistant_messages() {
        assert_eq!(
            assistant_copy_state(Some("Ship it"), true, false),
            AssistantCopyState {
                text: Some("Ship it".into()),
                visible: true,
            }
        );
    }

    #[test]
    fn hides_copy_while_an_assistant_message_is_still_streaming() {
        assert_eq!(
            assistant_copy_state(Some("Still streaming"), true, true),
            AssistantCopyState {
                text: Some("Still streaming".into()),
                visible: false,
            }
        );
    }

    #[test]
    fn hides_copy_for_empty_completed_assistant_messages() {
        assert_eq!(
            assistant_copy_state(Some("   "), true, false),
            AssistantCopyState {
                text: None,
                visible: false,
            }
        );
    }

    #[test]
    fn hides_copy_for_non_terminal_assistant_messages() {
        assert_eq!(
            assistant_copy_state(Some("Interim thought"), false, false),
            AssistantCopyState {
                text: Some("Interim thought".into()),
                visible: false,
            }
        );
    }

    #[test]
    fn says_an_empty_settled_response_is_empty() {
        let mut empty = message("");
        assert_eq!(assistant_display_text(&empty), "(empty response)");
        empty.streaming = true;
        assert_eq!(assistant_display_text(&empty), "");
        assert!(!assistant_meta(None, &empty, true, false).show_timestamp);
    }

    #[test]
    fn copies_the_rendered_representation_of_directives() {
        let text = [
            r#"Created :codex-file-citation{path="outputs/report.xlsx" purpose="output"}."#,
            "",
            r#"::artifact-template{skill_name="artifact-template-hello-world" skill_directory="/Users/test/.codex/skills/artifact-template-hello-world" display_name="Hello World" artifact_kind="document"}"#,
        ]
        .join("\n");
        assert_eq!(
            assistant_copy_state(Some(&text), true, false),
            AssistantCopyState {
                text: Some(
                    "Created [report.xlsx](<outputs/report.xlsx>).\n\nHello World (Document template)"
                        .into()
                ),
                visible: true,
            }
        );
    }
}
