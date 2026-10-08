//! The composer of a thread or of the new-thread draft, assembled from the
//! draft, the thread's requests and plan, the outbox and the model catalog.
use super::actions::{
    ComposerPrimaryAction, MobileSendInput, MobileSendPresentation, PendingAnswerProgress,
    PrimaryActionInput, can_interrupt_running_thread, can_resume, composer_primary_action,
    mobile_send_presentation, mobile_shows_stop, session_phase, thread_runtime,
};
use super::chips::{ContextChip, ContextChipSurface, context_chips};
use super::context_meter::{
    CompactControl, ContextWindowMeter, context_window_meter, context_window_model_display_name,
    latest_context_window,
};
use super::controls::{ComposerControls, ProviderControls, composer_controls};
use super::prompt::{
    ComposerEditor, EditorState, SubmissionTarget, composer_editor, has_sendable_content,
    submission_validation_message,
};
use super::stash::{StashEntryView, StashShortcut, stash_menu, stash_shortcut};
use crate::commands::outbox::Phase;
use crate::state::{CHATS_PROJECT, Draft, DraftAttachment, Snapshot};
use crate::view::attachments::{attachment_limit_error, file_attachment_block_reason};
use crate::view::models::{
    ModelCatalog, ProviderStatus, catalog,
    picker::{ModelPickerTrigger, trigger},
    traits::{TraitsView, build_traits},
};
use crate::view::plan::{PlanView, plan_view};
use crate::view::requests::{RequestsView, thread_requests_view};
use crate::view::setup_card::setup_progress;
use agent_domain::{State, ThreadId, ThreadShell};

/// Shortcut labels the desktop appends to tooltips; mobile leaves them empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ComposerShortcuts {
    /// The alternate send chord, as in "⌘⇧↵".
    pub alternate_send: Option<String>,
    /// Steers with the first queued message.
    pub queue_steer: Option<String>,
    /// Edits the last queued message.
    pub queue_edit: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ComposerOptions {
    /// The narrow layout's labels.
    pub compact: bool,
    /// The alternate send modifier (Ctrl/⌘) is held.
    pub alternate_modifier: bool,
    pub shortcuts: ComposerShortcuts,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ComposerView {
    /// The draft the composer edits; attachment intents name it.
    pub draft_key: String,
    pub text: String,
    /// A queued run loaded into the composer for editing.
    pub editing_queued_run: Option<String>,
    pub primary_action: ComposerPrimaryAction,
    pub mobile_send: MobileSendPresentation,
    pub mobile_shows_stop: bool,
    pub editor: ComposerEditor,
    pub controls: ComposerControls,
    pub model_trigger: ModelPickerTrigger,
    pub traits: TraitsView,
    /// Only for a thread that reported its context use.
    pub context_meter: Option<ContextWindowMeter>,
    pub context_chips: Vec<ContextChip>,
    pub attachments: Vec<DraftAttachment>,
    pub attachment_error: Option<String>,
    pub stash_shortcut: StashShortcut,
    pub stash: Vec<StashEntryView>,
    pub validation_message: Option<String>,
}

/// The draft the composer edits for `thread` (the new-thread draft for
/// `None`), with its key.
pub fn composer_draft(snapshot: &Snapshot, thread: Option<&ThreadId>) -> (String, Draft) {
    match thread {
        Some(id) if snapshot.selected_thread.as_ref() == Some(id) => {
            (snapshot.draft_key(), snapshot.current_draft())
        }
        Some(id) => (id.to_string(), snapshot.draft_for_thread(id)),
        None => {
            let key = snapshot.new_thread_draft_key();
            let draft = snapshot
                .drafts
                .get(&key)
                .cloned()
                .unwrap_or_else(|| snapshot.default_draft.user_defaults());
            (key, draft)
        }
    }
}

/// What a thread contributes to its composer.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ThreadComposer<'a> {
    pub thread: &'a ThreadId,
    pub state: Option<&'a State>,
    pub shell: Option<&'a ThreadShell>,
    pub requests: Option<&'a RequestsView>,
    pub plan: Option<&'a PlanView>,
    pub preparing_worktree: bool,
}

pub fn composer_view(
    snapshot: &Snapshot,
    thread: Option<&ThreadId>,
    options: &ComposerOptions,
) -> ComposerView {
    let (key, draft) = composer_draft(snapshot, thread);
    let Some(thread) = thread else {
        return assemble(snapshot, key, draft, None, options);
    };
    let state = snapshot.thread_state(thread);
    let shell = crate::view::thread::thread_shell(snapshot, thread);
    let requests = state.map(|state| thread_requests_view(snapshot, state));
    let plan =
        state.map(|state| plan_view(state, draft.interaction_mode, !draft.attachments.is_empty()));
    let context = ThreadComposer {
        thread,
        state,
        shell: shell.as_ref(),
        requests: requests.as_ref(),
        plan: plan.as_ref(),
        preparing_worktree: setup_progress(snapshot, thread).preparing_worktree,
    };
    assemble(snapshot, key, draft, Some(context), options)
}

fn provider_controls(catalog: &ModelCatalog, draft: &Draft) -> ProviderControls {
    ProviderControls {
        known: catalog
            .instance(&draft.instance_id)
            .is_some_and(|instance| instance.picker_ready()),
        supported_runtime_modes: catalog
            .instance(&draft.instance_id)
            .map(|instance| instance.supported_runtime_modes.clone())
            .unwrap_or_default(),
        shows_interaction_toggle: catalog
            .instance(&draft.instance_id)
            .map(|instance| instance.show_interaction_mode_toggle),
        model_label: catalog
            .models_of(&draft.instance_id)
            .find(|model| model.slug == draft.model)
            .map(|model| model.name.clone()),
    }
}

/// "Compact context" in the context meter: offered when the provider has a
/// `/compact` command for the composer's directory, and disabled while the
/// thread is busy or has nothing to compact.
fn compact_control(
    snapshot: &Snapshot,
    draft: &Draft,
    thread: Option<&ThreadComposer<'_>>,
    busy: bool,
) -> CompactControl {
    let available = snapshot
        .sources
        .provider_commands(&draft.instance_id, &snapshot.composer_cwd())
        .is_some_and(|commands| {
            commands
                .slash_commands
                .iter()
                .any(|command| command.name == "compact")
        });
    let compactable = thread.is_some_and(|thread| {
        thread.state.is_some_and(|state| {
            crate::view::composer::commands::has_compactable_conversation(
                state,
                snapshot
                    .thread(thread.thread)
                    .is_some_and(|sync| sync.history.has_more),
                thread
                    .shell
                    .and_then(|shell| shell.latest_user_message_at.as_ref()),
            )
        })
    });
    let has_project = thread.is_some_and(|thread| {
        thread
            .state
            .and_then(|state| state.thread.as_ref())
            .is_some_and(|current| {
                snapshot
                    .shell_projects()
                    .iter()
                    .any(|project| project.id == current.project)
            })
    });
    let disabled = !available
        || !has_project
        || !compactable
        || busy
        || thread.is_some_and(|thread| thread.preparing_worktree);
    CompactControl {
        available,
        disabled,
        disabled_reason: disabled.then(|| {
            if !has_project {
                "Choose a project before compacting"
            } else if !available {
                "Compaction is unavailable for this provider"
            } else {
                "Compacting is unavailable right now"
            }
            .into()
        }),
    }
}

pub(crate) fn assemble(
    snapshot: &Snapshot,
    draft_key: String,
    draft: Draft,
    thread: Option<ThreadComposer<'_>>,
    options: &ComposerOptions,
) -> ComposerView {
    let catalog = catalog(snapshot);
    let provider = provider_controls(&catalog, &draft);
    let state = thread.and_then(|thread| thread.state);
    let runtime = thread
        .and_then(|thread| thread.shell)
        .and_then(thread_runtime);
    let editing = thread
        .filter(|thread| snapshot.selected_thread.as_ref() == Some(thread.thread))
        .and(snapshot.editing_run.as_ref());
    let questions = thread
        .and_then(|thread| thread.requests)
        .and_then(|requests| requests.questions.as_ref());
    let approval_pending = thread
        .and_then(|thread| thread.requests)
        .is_some_and(|requests| requests.approval.is_some());
    let plan_follow_up = thread
        .and_then(|thread| thread.plan)
        .is_some_and(|plan| plan.show_plan_follow_up_prompt);
    let is_running = state.is_some_and(|state| state.active_run().is_some());
    let can_interrupt = can_interrupt_running_thread(thread.is_some(), runtime.as_ref());
    let context_count = draft
        .context
        .as_ref()
        .map_or(0, |context| context.records.len());
    let sendable = has_sendable_content(&draft.text, draft.attachments.len(), context_count);
    let validation_message = submission_validation_message(
        &draft.text,
        None,
        if questions.is_some() {
            SubmissionTarget::PendingUserInput
        } else {
            SubmissionTarget::ProviderTurn
        },
    );
    let attachment_error = attachment_limit_error(&draft.attachments)
        .or_else(|| file_attachment_block_reason(&draft.attachments));
    let send_disabled_reason = validation_message
        .clone()
        .or_else(|| attachment_error.clone())
        .or_else(|| {
            (!draft.attachments.is_empty())
                .then(|| draft.attachment_refs().err())
                .flatten()
        });
    let primary_action = composer_primary_action(&PrimaryActionInput {
        compact: options.compact,
        pending_answer: questions.map(|questions| PendingAnswerProgress {
            question_index: questions.progress.question_index,
            is_last_question: questions.progress.is_last_question,
            can_advance: questions.progress.can_advance,
            is_responding: questions.responding,
            is_complete: questions.progress.is_complete,
        }),
        is_running,
        can_interrupt,
        follow_up: snapshot.follow_up,
        alternate_modifier: options.alternate_modifier,
        alternate_shortcut_label: options.shortcuts.alternate_send.clone(),
        show_plan_follow_up_prompt: plan_follow_up,
        prompt_has_text: !draft.text.trim().is_empty(),
        // A send leaves the composer at once; the outbox carries it.
        is_send_busy: false,
        send_disabled_reason,
        // The outbox holds sends while the Host is unreachable.
        is_connecting: false,
        is_environment_unavailable: false,
        is_preparing_worktree: thread.is_some_and(|thread| thread.preparing_worktree),
        has_sendable_content: sendable,
        can_resume: state.is_some_and(can_resume),
        is_editing_queued_message: editing.is_some(),
    });
    let delivery_deferred = !snapshot.connected
        || thread.is_some_and(|thread| {
            snapshot
                .outbox
                .entries
                .iter()
                .any(|entry| &entry.thread == thread.thread && entry.phase == Phase::Queued)
        });
    let mobile_send = mobile_send_presentation(MobileSendInput {
        editing_queued_message: editing.is_some(),
        running: is_running,
        can_steer: agent_domain::TurnSupport::for_driver(draft.driver).steer,
        follow_up: snapshot.follow_up,
        delivery_deferred,
    });
    let projects = snapshot.shell_projects();
    let target_project = if thread.is_none() {
        snapshot.new_thread_project_id().unwrap_or(CHATS_PROJECT)
    } else {
        snapshot
            .selected_project
            .as_deref()
            .unwrap_or(CHATS_PROJECT)
    };
    let editor = composer_editor(&EditorState {
        pending_approval: approval_pending,
        // Every question also takes a typed answer.
        pending_question_choice_only: questions.map(|_| false),
        pending_answer_responding: questions.is_some_and(|questions| questions.responding),
        plan_follow_up_with_plan: plan_follow_up,
        project_selection_required: thread.is_none()
            && !projects.iter().any(|project| project.id == target_project),
        provider_unavailable: !catalog.instances.is_empty()
            && catalog
                .instances
                .iter()
                .all(|instance| instance.status == ProviderStatus::Error),
        is_connecting: false,
        phase: thread.map(|_| session_phase(runtime.as_ref())),
    });
    let compact = compact_control(
        snapshot,
        &draft,
        thread.as_ref(),
        is_running || approval_pending || questions.is_some() || plan_follow_up,
    );
    let context_meter = state.and_then(latest_context_window).map(|usage| {
        let name = context_window_model_display_name(&draft.model, provider.model_label.as_deref());
        context_window_meter(&usage, Some(&name), &compact)
    });
    let threads = snapshot
        .shell_view()
        .map(|shell| shell.threads.clone())
        .unwrap_or_default();
    let attachments: Vec<_> = draft
        .attachments
        .iter()
        .map(DraftAttachment::metadata)
        .collect();
    ComposerView {
        context_chips: context_chips(
            &draft.text,
            draft.context.as_ref(),
            &attachments,
            &threads,
            ContextChipSurface::Composer,
        ),
        draft_key,
        editing_queued_run: editing.map(ToString::to_string),
        primary_action,
        mobile_shows_stop: mobile_shows_stop(sendable, can_interrupt, editing.is_some()),
        mobile_send,
        editor,
        // The Host has no plan-mode setting; Plan is always offered.
        controls: composer_controls(&draft, &provider, true),
        model_trigger: trigger(&catalog, &draft.instance_id, &draft.model),
        traits: build_traits(&catalog, &draft, true),
        context_meter,
        attachment_error,
        stash_shortcut: stash_shortcut(&draft, &snapshot.stash),
        stash: stash_menu(&snapshot.stash),
        validation_message,
        text: draft.text,
        attachments: draft.attachments,
    }
}
