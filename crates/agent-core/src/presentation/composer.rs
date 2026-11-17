//! A composer's own inputs and controls, independent of transcript deltas.
use super::model_settings::ModelQuickControls;
use super::queue::{QueuePresentation, QueuedMessage};
use crate::{
    models::{Model, ModelRef},
    session::ProviderKind,
    state::{
        ComposerPreferences, Draft, Navigation, Same, Snapshot, follow_up,
        operations::{OperationKey, OperationPhase},
    },
};
use agent_protocol::{ids::ClientInputId, operations::FollowUp};
use std::sync::{Arc, OnceLock};

/// What T3 Code's Resume sends to continue an interrupted run.
pub const RESUME_PROMPT: &str = "Continue where you left off.";

pub mod footer;
mod workspace;
pub use workspace::WorkspaceSelection;
#[cfg(test)]
mod queue_controls_tests;
#[cfg(test)]
mod tests;

/// T3 Code's `deriveResumableRunId`: the latest turn was interrupted or failed
/// on the provider's usage limit.
fn resumable(thread: &crate::models::Thread) -> bool {
    use agent_protocol::execution::{ErrorCategory, TurnStatus};
    let Some(turn) = thread.turns.as_ref().and_then(|turns| turns.last()) else {
        return false;
    };
    match turn.status {
        TurnStatus::Interrupted => true,
        TurnStatus::Failed => turn
            .error
            .as_ref()
            .is_some_and(|error| error.category == ErrorCategory::UsageLimit),
        _ => false,
    }
}

#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ComposerControls {
    pub triage_available: bool,
    pub project_clone: Option<super::project_clone::ProjectCloneView>,
    pub fresh_context_available: bool,
    pub feedback_available: bool,
    pub feedback: Option<super::feedback::FeedbackNotice>,
    pub workspace: Option<WorkspaceSelection>,
    pub navigation: Navigation,
    pub connected: bool,
    /// The selected conversation is loaded, or the draft starts a new one. One
    /// still opening, or whose read failed, takes typing but nothing that would
    /// send into it: no message, recording or attachment.
    pub loaded: bool,
    pub provider: Option<ProviderKind>,
    pub selected_model: Option<ModelRef>,
    pub quick: ModelQuickControls,
    pub sending: bool,
    pub transferring: bool,
    pub transcribing: bool,
    pub running_turn: Option<agent_protocol::ids::TurnId>,
    /// The work the answered running turn waits on. As in T3 Code, the
    /// composer sends while it waits; the work's own strip offers Stop.
    pub background: Option<super::background_work::BackgroundWork>,
    pub interrupt_phase: Option<OperationPhase>,
    pub unavailable: Option<String>,
    pub question: Option<crate::questions::QuestionPrompt>,
    /// Where the docked question's answer is in its delivery.
    pub question_phase: Option<OperationPhase>,
    pub follow_up: FollowUpControls,
}

/// What sending does while a turn runs and the messages waiting behind it, as
/// The composer's follow-up shortcuts and the shared Host queue.
#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct FollowUpControls {
    pub preferences: ComposerPreferences,
    /// A send now goes into or behind a running turn, or behind waiting messages.
    pub active: bool,
    /// What a plain send does, and what the alternate does, as the provider allows.
    pub default_action: FollowUp,
    pub alternate_action: FollowUp,
    pub queue: Arc<QueuePresentation>,
}

/// T3's queue flattens context references to their labels instead of rendering chips.
fn queued_preview<'a>(
    text: &'a str,
    contexts: impl IntoIterator<Item = &'a agent_protocol::context::MessageContext>,
) -> String {
    std::iter::once(text)
        .chain(contexts.into_iter().map(|context| context.label.as_str()))
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn follow_up_controls(
    snapshot: &Snapshot,
    thread: Option<&crate::models::Thread>,
    fresh_context: bool,
    edit_blocked: bool,
) -> FollowUpControls {
    let preferences = snapshot.input.composer_preferences;
    let action = |alternate| {
        let requested = follow_up::for_context(
            follow_up::requested(preferences.follow_up, alternate),
            fresh_context,
        );
        if thread.is_some_and(|thread| follow_up::waits(thread, requested)) {
            FollowUp::Queue
        } else {
            FollowUp::Steer
        }
    };
    let Some(thread) = thread else {
        return FollowUpControls {
            preferences,
            active: false,
            default_action: preferences.follow_up,
            alternate_action: follow_up::requested(preferences.follow_up, true),
            queue: QueuePresentation::empty(snapshot.catalog.ui_language),
        };
    };
    let session = thread.id.as_ref();
    let busy = session.is_some_and(|session| {
        snapshot.operation_running(OperationKey::Queue {
            session: session.clone(),
        })
    });
    let waiting = thread.queue.inputs.iter().map(|input| {
        let text = follow_up::queued_text(input);
        let contexts = agent_protocol::operations::Input::contexts(&input.input);
        QueuedMessage {
            id: input.client_user_message_id.clone(),
            fresh_context: input.fresh_context,
            text: queued_preview(&text, contexts),
            attachments: input
                .input
                .iter()
                .filter(|part| {
                    !matches!(
                        part,
                        agent_protocol::operations::Input::Text { .. }
                            | agent_protocol::operations::Input::Context { .. }
                    )
                })
                .count() as u32,
            pending: false,
        }
    });
    let key = session.map(crate::state::DraftKey::from);
    let mut sending: Vec<_> = snapshot
        .sessions
        .pending_submissions
        .iter()
        .filter(|(id, pending)| {
            pending.queued
                && !pending.accepted
                && Some(&pending.draft_key) == key.as_ref()
                && !thread
                    .queue
                    .inputs
                    .iter()
                    .any(|input| &input.client_user_message_id == *id)
        })
        .collect();
    sending.sort_by_key(|(_, pending)| pending.sequence);
    FollowUpControls {
        preferences,
        active: follow_up::working(thread)
            || (!thread.queue.held && !thread.queue.inputs.is_empty()),
        default_action: action(false),
        alternate_action: action(true),
        queue: Arc::new(QueuePresentation {
            messages: waiting
                .chain(sending.into_iter().map(|(id, pending)| QueuedMessage {
                    id: id.clone(),
                    fresh_context: pending.draft.fresh_context,
                    text: queued_preview(&pending.draft.text, &pending.draft.contexts),
                    attachments: pending.draft.attachments.len() as u32,
                    pending: true,
                }))
                .collect(),
            held: thread.queue.held,
            resumable: resumable(thread),
            can_steer: thread.steerable || thread.skips_compaction,
            editing: session.and_then(|session| {
                snapshot
                    .input
                    .queued_edits
                    .get(session)
                    .map(|edit| edit.client_user_message_id.clone())
            }),
            busy,
            language: snapshot.catalog.ui_language,
            session: session.cloned(),
            skips_compaction: thread.skips_compaction,
            edit_blocked,
            can_change: snapshot.connection.connected
                && crate::session::direct_input_unavailable_reason(thread.can_accept_direct_input)
                    .is_none(),
        }),
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct ComposerPresentation {
    language: crate::localization::UiLanguage,
    pub draft: Arc<Draft>,
    pub controls: ComposerControls,
    models: Arc<Vec<Model>>,
    catalog: Option<Arc<agent_protocol::composer::ComposerCatalog>>,
    paths: Option<Arc<crate::composer::completions::PathSearch>>,
    stashes: Arc<Vec<crate::composer::stash::StashedPrompt>>,
}

impl ComposerPresentation {
    pub(crate) fn equivalent(&self, other: &Self) -> bool {
        self.language == other.language
            && self.draft.same(&other.draft)
            && self.controls == other.controls
            && self.models.same(&other.models)
            && self.catalog.same(&other.catalog)
            && self.paths.same(&other.paths)
            && self.stashes.same(&other.stashes)
    }
    pub(crate) fn project(
        snapshot: &Snapshot,
        pane: &crate::state::PaneState,
        previous: Option<&Arc<Self>>,
    ) -> Arc<Self> {
        static EMPTY: OnceLock<Arc<Draft>> = OnceLock::new();
        let mut navigation = pane.navigation.as_ref().clone();
        navigation.draft_key =
            snapshot.composer_key(navigation.thread_id.as_ref(), &navigation.draft_key);
        let key = &navigation.draft_key;
        let draft = snapshot
            .input
            .drafts
            .get(key)
            .cloned()
            .unwrap_or_else(|| EMPTY.get_or_init(Arc::default).clone());
        let session = pane.navigation.thread_id.as_ref();
        let thread = session.and_then(|id| snapshot.sessions.conversations.get(id));
        let running_turn = thread.and_then(|thread| thread.active_turn_id());
        let provider = snapshot.model_provider_for_draft(key.clone());
        let question = session.and_then(|id| snapshot.question_prompt(id.clone()));
        let controls = ComposerControls {
            triage_available: snapshot.connection.connected
                && !snapshot.operation_running(OperationKey::Triage),
            project_clone: snapshot.project_clone_views().into_iter().find(|entry| {
                entry.state.request.destination == pane.navigation.cwd && entry.blocks_send
            }),
            fresh_context_available: session
                .is_some_and(|id| snapshot.can_set_fresh_context(id, !draft.fresh_context)),
            feedback_available: session.is_some_and(|id| snapshot.feedback_available(id.clone())),
            feedback: session.and_then(|id| snapshot.feedback_notice(id.clone())),
            workspace: workspace::project(snapshot, pane, &draft),
            navigation: navigation.clone(),
            connected: snapshot.connection.connected,
            loaded: session.is_none() || thread.is_some(),
            provider,
            selected_model: draft.model.clone().or_else(|| {
                provider.and_then(|provider| snapshot.model_for_provider(key.clone(), provider))
            }),
            quick: snapshot.model_quick_controls(key.clone()),
            sending: snapshot.operation_running(OperationKey::Submission {
                draft_key: key.clone(),
            }),
            transferring: snapshot.operation_running(OperationKey::Attachment {
                draft_key: key.clone(),
            }),
            transcribing: snapshot.operation_running(OperationKey::Dictation {
                draft_key: key.clone(),
            }),
            interrupt_phase: session
                .zip(running_turn.as_ref())
                .and_then(|(session, turn)| {
                    snapshot.operation_phase(OperationKey::Interrupt {
                        session: session.clone(),
                        turn: turn.clone(),
                    })
                }),
            running_turn,
            background: thread.and_then(|thread| {
                super::background_work::BackgroundWork::new(
                    &thread.background,
                    follow_up::working(thread),
                )
            }),
            unavailable: session
                .and_then(|id| snapshot.input_unavailable_reason(id.clone(), provider))
                .or_else(|| {
                    snapshot
                        .project_clone_block_reason(&pane.navigation.cwd)
                        .map(|reason| snapshot.catalog.ui_language.text(&reason).to_owned())
                }),
            question_phase: question.as_ref().and_then(|question| {
                snapshot.operation_phase(OperationKey::Request {
                    id: question.request_id.clone(),
                })
            }),
            follow_up: follow_up_controls(
                snapshot,
                thread.map(AsRef::as_ref),
                draft.fresh_context,
                question.is_some(),
            ),
            question,
        };
        let catalog = snapshot
            .catalog
            .composer_catalogs
            .get(&pane.navigation.cwd)
            .cloned();
        let next = Self {
            language: snapshot.catalog.ui_language,
            draft,
            controls,
            models: snapshot.catalog.models.clone(),
            catalog,
            paths: pane.composer_paths.clone(),
            stashes: snapshot.input.stashes.clone(),
        };
        if let Some(previous) = previous
            && next.equivalent(previous)
        {
            return previous.clone();
        }
        Arc::new(next)
    }

    pub fn completion_source(&self) -> crate::composer::completions::CompletionSource {
        crate::composer::completions::CompletionSource {
            connected: self.controls.connected,
            catalog: self.completion_catalog(),
            draft: self.controls.navigation.draft_key.clone(),
            root: crate::composer::completions::path_root(
                &self.controls.navigation.cwd,
                self.controls.navigation.thread_id.is_some(),
                self.draft.workspace.as_ref(),
            )
            .into(),
            paths: self.paths.clone(),
        }
    }
}

/// The workspace changes a conversation's composer offers to review.
///
/// This is the shared summary used by native composer adapters. The full file
/// list and diff stay in `WorkspaceReview`; the composer only needs the counts
/// that the iOS badge and the desktop card display.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ComposerReview {
    pub files: u64,
    pub additions: u64,
    pub deletions: u64,
}

impl crate::state::PaneState {
    /// The summary for an open conversation's workspace changes; none for a
    /// side chat, draft, or clean workspace.
    pub fn composer_review(&self, is_side_chat: bool) -> Option<ComposerReview> {
        if is_side_chat {
            return None;
        }
        self.navigation.thread_id.as_ref()?;
        let review = self
            .workspace
            .review
            .as_ref()
            .filter(|review| !review.files.is_empty())?;
        Some(ComposerReview {
            files: review.files.len() as u64,
            additions: review.additions,
            deletions: review.deletions,
        })
    }
}

impl Snapshot {
    pub(crate) fn submission_unavailable_reason(
        &self,
        session: &crate::session::SessionRef,
        selected: Option<ProviderKind>,
    ) -> Option<String> {
        let thread = self.thread_metadata(session)?;
        crate::session::direct_input_unavailable_reason(thread.can_accept_direct_input).or_else(|| {
            let active = thread.active_provider.or(session.provider);
            let busy = agent_protocol::models::task_active(None, thread.status)
                || thread.active_turn_id().is_some()
                || !thread.requests.is_empty()
                || !thread.preparations.is_empty();
            if selected.is_some_and(|provider| Some(provider) != active) && busy {
                Some("AIを切り替えて送信するには、現在の応答・確認・送信の完了を待つか、新しいチャットを開いてください。".into())
            } else {
                None
            }
        })
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn input_unavailable_reason(
        &self,
        session: crate::session::SessionRef,
        selected: Option<ProviderKind>,
    ) -> Option<String> {
        self.submission_unavailable_reason(&session, selected)
            .map(|reason| self.catalog.ui_language.text(&reason).to_owned())
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl ComposerPresentation {
    /// Transfers, dictation and queue editing own draft content until they finish.
    /// Native recording or marked text is supplied by the input owner.
    pub fn can_restore_stash(&self, native_input_busy: bool) -> bool {
        !native_input_busy
            && !self.controls.transferring
            && !self.controls.transcribing
            && self.controls.follow_up.queue.editing.is_none()
    }
    pub fn can_stash(&self, native_input_busy: bool) -> bool {
        self.can_restore_stash(native_input_busy)
            && self.stashes.len() < crate::composer::stash::STASH_LIMIT
            && crate::composer::stash::has_content(&self.draft)
    }
    pub fn stashed_prompts(&self) -> Vec<crate::composer::stash::StashedPrompt> {
        self.stashes.as_ref().clone()
    }
    pub fn stash_count(&self) -> u32 {
        self.stashes.len() as u32
    }
    pub fn draft(&self) -> Draft {
        self.draft.as_ref().clone()
    }
    pub fn controls(&self) -> ComposerControls {
        self.controls.clone()
    }
    pub fn current_model(&self) -> Option<Model> {
        super::model_settings::current_model(&self.models, self.draft.model.as_ref()).cloned()
    }
    pub fn can_choose_model(&self) -> bool {
        self.controls.connected
            && !self.controls.sending
            && self
                .controls
                .navigation
                .draft_key
                .owns_submission_settings()
    }
    /// Answered background work has its own stop in the conversation footer.
    pub fn primary_stop_turn(&self) -> Option<agent_protocol::ids::TurnId> {
        self.controls
            .background
            .is_none()
            .then(|| self.controls.running_turn.clone())
            .flatten()
    }
    pub fn can_interrupt(&self, turn: Option<agent_protocol::ids::TurnId>) -> bool {
        self.controls.connected
            && turn.is_some()
            && self.controls.running_turn == turn
            && self.controls.interrupt_phase != Some(OperationPhase::Running)
    }
    pub fn can_answer_question(&self) -> bool {
        super::context::request_answer_enabled(
            self.controls.connected,
            self.controls
                .question
                .as_ref()
                .is_some_and(|question| question.can_respond),
            self.controls.question_phase.clone(),
            false,
        )
    }
    /// Editing, choosing and advancing share the same response-delivery boundary.
    pub fn can_advance_question(&self) -> bool {
        self.can_answer_question()
            && self
                .controls
                .question
                .as_ref()
                .is_some_and(|question| question.can_advance)
    }
    pub fn composer_suggestions(
        &self,
        text: String,
        cursor: u32,
    ) -> Option<crate::composer::ComposerSuggestions> {
        self.completion_catalog().suggestions(&text, cursor)
    }
    pub fn completion_catalog(&self) -> crate::composer::CompletionCatalog {
        crate::composer::completion_catalog(
            self.language,
            self.catalog.as_deref(),
            self.controls.provider,
        )
    }
    pub fn unchanged(&self, other: Arc<Self>) -> bool {
        self.equivalent(&other)
    }
    /// Whether core state lets the composer send `has_content`. Clients add
    /// their own media transfer, recording, and permission state.
    pub fn can_send(&self, has_content: bool) -> bool {
        (has_content || !self.draft.contexts.is_empty())
            && self.accepts_input()
            && !(self.controls.follow_up.queue.editing.is_some()
                && self.controls.follow_up.queue.busy)
            && self.controls.provider.is_some()
            && self.controls.unavailable.is_none()
    }
    /// Transcription only fills the draft, so it does not wait for a provider.
    pub fn can_dictate(&self) -> bool {
        self.accepts_input()
    }
    pub fn can_attach(&self) -> bool {
        self.accepts_input()
    }

    pub fn can_resume(&self) -> bool {
        let follow_up = &self.controls.follow_up;
        self.can_send(true)
            && self.controls.navigation.thread_id.is_some()
            && self.controls.running_turn.is_none()
            && self.controls.question.is_none()
            && self.draft.text.trim().is_empty()
            && self.draft.attachments.is_empty()
            && self.draft.contexts.is_empty()
            && follow_up.queue.editing.is_none()
            && !follow_up.queue.busy
            && (follow_up.queue.resumable
                || follow_up.queue.held && !follow_up.queue.messages.is_empty())
    }

    /// Capture the current input owner. A queued-save callback can never turn
    /// into a normal send when its editor is cancelled or the queue advances.
    pub fn submit_intent(
        &self,
        client_user_message_id: ClientInputId,
        alternate: bool,
    ) -> crate::state::Intent {
        if matches!(
            self.controls.navigation.draft_key,
            crate::state::DraftKey::Queued { .. }
        ) {
            crate::state::Intent::SaveQueuedEdit {
                draft_key: self.controls.navigation.draft_key.clone(),
            }
        } else {
            crate::state::Intent::Submit {
                thread_id: self.controls.navigation.thread_id.clone(),
                client_user_message_id,
                alternate,
            }
        }
    }

    /// The owner dispatches these in order. Held input resumes in its original
    /// conversation; an interrupted run without queued input gets one continuation.
    pub fn resume_intents(
        &self,
        client_user_message_id: ClientInputId,
    ) -> Vec<crate::state::Intent> {
        use crate::state::{Intent, follow_up::QueueAction};
        if !self.can_resume() {
            return Vec::new();
        }
        let thread_id = self.controls.navigation.thread_id.clone().unwrap();
        if self.controls.follow_up.queue.held && !self.controls.follow_up.queue.messages.is_empty()
        {
            return vec![Intent::Queue {
                thread_id,
                action: QueueAction::Resume,
            }];
        }
        vec![
            Intent::EditComposer {
                thread_id: self.controls.navigation.draft_key.clone(),
                text: RESUME_PROMPT.into(),
                cursor: RESUME_PROMPT.len() as u32,
            },
            Intent::Submit {
                thread_id: Some(thread_id),
                client_user_message_id,
                alternate: false,
            },
        ]
    }
}

impl ComposerPresentation {
    fn accepts_input(&self) -> bool {
        let controls = &self.controls;
        controls.connected
            && controls.loaded
            && !controls.sending
            && !controls.transferring
            && !controls.transcribing
    }
}

#[cfg(test)]
mod availability_tests {
    use super::*;
    use crate::state::DraftKey;

    fn presentation(edit: impl FnOnce(&mut Snapshot)) -> Arc<ComposerPresentation> {
        let mut snapshot = Snapshot::default();
        snapshot.connection.connected = true;
        Arc::make_mut(&mut snapshot.catalog.models).push(
            serde_json::from_value(serde_json::json!({
                "id":"gpt","model":{"provider":"codex","id":"gpt"},"displayName":"GPT"
            }))
            .unwrap(),
        );
        let key = snapshot.panes.primary().navigation.draft_key.clone();
        Arc::make_mut(&mut snapshot.input.drafts).insert(
            key,
            Arc::new(Draft {
                model: Some(ModelRef {
                    provider: ProviderKind::Codex,
                    id: "gpt".into(),
                }),
                ..Default::default()
            }),
        );
        edit(&mut snapshot);
        ComposerPresentation::project(&snapshot, snapshot.panes.primary(), None)
    }

    fn running(snapshot: &mut Snapshot, key: OperationKey) {
        Arc::make_mut(&mut snapshot.operations).insert(
            key,
            crate::state::operations::OperationState {
                generation: 1,
                phase: OperationPhase::Running,
            },
        );
    }

    #[test]
    fn a_ready_composer_sends_content_and_accepts_media() {
        let ready = presentation(|_| ());
        assert!(ready.can_send(true));
        assert!(!ready.can_send(false));
        assert!(ready.can_dictate() && ready.can_attach());
    }

    #[test]
    fn stash_controls_preserve_content_owned_by_transfers_recording_and_queue_edits() {
        let empty = presentation(|_| ());
        assert!(!empty.can_stash(false));
        assert!(empty.can_restore_stash(false));
        let ready = presentation(|snapshot| {
            let key = snapshot.panes.primary().navigation.draft_key.clone();
            Arc::make_mut(
                Arc::make_mut(&mut snapshot.input.drafts)
                    .get_mut(&key)
                    .unwrap(),
            )
            .text = "keep this prompt".into();
        });
        assert!(ready.can_stash(false));
        assert!(!ready.can_stash(true));
        assert!(!ready.can_restore_stash(true));
        for key in [
            OperationKey::Attachment {
                draft_key: DraftKey::default(),
            },
            OperationKey::Dictation {
                draft_key: DraftKey::default(),
            },
        ] {
            let blocked = presentation(|snapshot| running(snapshot, key));
            assert!(!blocked.can_restore_stash(false));
        }
        let mut editing = Arc::try_unwrap(ready).ok().unwrap();
        Arc::make_mut(&mut editing.controls.follow_up.queue).editing = Some("queued".into());
        assert!(!editing.can_stash(false));
        assert!(!editing.can_restore_stash(false));
        Arc::make_mut(&mut editing.controls.follow_up.queue).editing = None;
        let mut full = editing;
        full.stashes = Arc::new(
            (0..crate::composer::stash::STASH_LIMIT)
                .map(|index| crate::composer::stash::StashedPrompt {
                    id: index.to_string(),
                    created_at_ms: 0,
                    text: "saved".into(),
                    attachments: vec![],
                    invocations: vec![],
                    contexts: vec![],
                })
                .collect(),
        );
        assert!(!full.can_stash(false));
        assert!(full.can_restore_stash(false));
        Arc::make_mut(&mut full.stashes).pop();
        assert!(full.can_stash(false));
    }

    #[test]
    fn triage_requires_a_connection_and_no_preparation_in_flight() {
        assert!(presentation(|_| ()).controls.triage_available);
        assert!(
            !presentation(|snapshot| snapshot.connection.connected = false)
                .controls
                .triage_available
        );
        assert!(
            !presentation(|snapshot| running(snapshot, OperationKey::Triage))
                .controls
                .triage_available
        );
    }

    #[test]
    fn core_activity_blocks_every_input_until_it_finishes() {
        fn draft(snapshot: &Snapshot) -> DraftKey {
            snapshot.panes.primary().navigation.draft_key.clone()
        }
        type Edit = fn(&mut Snapshot);
        let blocked: [(&str, Edit); 4] = [
            ("disconnected", |s| s.connection.connected = false),
            ("conversation not loaded", |s| {
                Arc::make_mut(&mut Arc::make_mut(s.panes.primary_mut()).navigation).thread_id =
                    Some(crate::session::SessionRef {
                        provider: Some(ProviderKind::Codex),
                        id: "unloaded".into(),
                    });
            }),
            ("sending", |s| {
                let draft_key = draft(s);
                running(s, OperationKey::Submission { draft_key });
            }),
            ("transcribing", |s| {
                let draft_key = draft(s);
                running(s, OperationKey::Dictation { draft_key });
            }),
        ];
        for (name, edit) in blocked {
            let composer = presentation(edit);
            assert!(!composer.can_send(true), "{name}");
            assert!(!composer.can_dictate(), "{name}");
            assert!(!composer.can_attach(), "{name}");
        }
    }

    #[test]
    fn sending_alone_needs_a_provider() {
        let composer = presentation(|snapshot| {
            Arc::make_mut(&mut snapshot.input.drafts).clear();
            snapshot.catalog.models = Arc::default();
        });
        assert!(composer.controls.provider.is_none());
        assert!(!composer.can_send(true));
        assert!(composer.can_dictate() && composer.can_attach());
    }
}
