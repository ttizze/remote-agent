//! Native intents: device edits apply at once; Host changes go through the
//! outbox or a request.
use super::{
    Outcome,
    owner::{Event, Owner, Waiter, new_id},
};
use crate::{
    commands::{
        build::*,
        lifecycle::LifecycleOverlay,
        outbox::{PendingCommand, Request, Restore},
        workflows::{latest_merge_back_run, queue_workflow, sort_pinned_by_order},
    },
    peer::PeerError,
    protocol::Call,
    state::*,
    view::attachments::{
        AttachmentCandidate, AttachmentFileKind, admit_attachments, image_preparation_error,
    },
};
use agent_domain::{
    ApprovalDecision, Command, InteractionMode, MessageId, Plan, PlanRef, RunId, RuntimeRequestId,
    State, ThreadId, Timestamp,
};
use agent_protocol::{conversation as c, models as m, operations as op};
use std::sync::Arc;

pub(super) enum Next {
    Done,
    /// Outbox entries in order; the last one resolves the intent.
    Commands(Vec<PendingCommand>),
    /// A request whose reply resolves the intent.
    Call(Box<Call>, Option<Box<(String, Draft)>>),
    /// Applied at once with this outcome.
    Outcome(Outcome),
}
impl Next {
    pub(super) fn call(call: Call, sent: Option<(String, Draft)>) -> Self {
        Self::Call(Box::new(call), sent.map(Box::new))
    }
}

pub(super) fn invalid(error: impl std::fmt::Display) -> PeerError {
    super::invalid(error)
}
fn thread_id(value: String) -> Result<ThreadId, PeerError> {
    ThreadId::new(value).map_err(invalid)
}
fn run_id(value: String) -> Result<RunId, PeerError> {
    RunId::new(value).map_err(invalid)
}
fn approval_decision(value: &str) -> Result<ApprovalDecision, PeerError> {
    Ok(match value {
        "accept" => ApprovalDecision::Accept,
        "acceptForSession" => ApprovalDecision::AcceptForSession,
        "acceptAlways" => ApprovalDecision::AcceptAlways,
        "decline" => ApprovalDecision::Decline,
        "cancel" => ApprovalDecision::Cancel,
        _ => return Err(invalid("Unknown approval decision")),
    })
}

/// The proposed plan the composer offers to implement or refine, by the
/// plan follow-up rules of the open thread and its draft.
pub(super) fn actionable_plan(snapshot: &Snapshot) -> Option<&Plan> {
    let view = crate::view::plan::selected_plan_view(snapshot)?;
    let id = view
        .active_proposed_plan
        .filter(|_| view.show_plan_follow_up_prompt)?
        .id;
    snapshot
        .selected_state()?
        .plans
        .iter()
        .find(|plan| plan.id.as_str() == id)
}

impl Owner {
    pub(super) fn intent(&mut self, intent: Intent, complete: Waiter) {
        match intent {
            Intent::PairRemoteHost { invitation, name } => {
                self.pair(invitation, name, complete);
                return;
            }
            Intent::ImportSessions => {
                self.import_sessions(complete);
                return;
            }
            _ => {}
        }
        let undoing = matches!(intent, Intent::UndoThreadAction);
        match self.prepare(intent) {
            Err(error) => {
                self.state.error = Some(crate::presentation::error::error_message(
                    &error.to_string(),
                ));
                let _ = complete.send(Err(error));
            }
            Ok(Next::Done) => {
                let _ = complete.send(Ok(Outcome::Applied));
            }
            Ok(Next::Outcome(outcome)) => {
                let _ = complete.send(Ok(outcome));
            }
            Ok(Next::Commands(mut entries)) => {
                if !undoing {
                    for entry in &entries {
                        self.claim_undo(entry);
                    }
                }
                let Some(last) = entries.pop() else {
                    let _ = complete.send(Ok(Outcome::Applied));
                    return;
                };
                for entry in entries {
                    if let Err(error) = self.enqueue(entry, None) {
                        let _ = complete.send(Err(error));
                        return;
                    }
                }
                let restore = last.restore.clone();
                if let Err(error) = self.enqueue(last, Some(complete)) {
                    if let Some(restore) = restore {
                        self.restore_draft(&restore);
                    }
                    self.state.error = Some(error.to_string());
                }
            }
            Ok(Next::Call(call, sent)) => self.job(*call, Some(complete), sent.map(|sent| *sent)),
        }
    }

    fn restore_draft(&mut self, restore: &Restore) {
        let mut draft = self
            .state
            .drafts
            .get(&restore.draft_key)
            .cloned()
            .unwrap_or_else(|| self.state.current_draft());
        draft.text = merge_restored_text(&draft.text, &restore.text);
        self.state.drafts.insert(restore.draft_key.clone(), draft);
    }

    pub(super) fn command(&self, thread: ThreadId, command: Command) -> PendingCommand {
        self.pending(thread, command)
    }

    pub(super) fn lifecycle(&self, thread: ThreadId, action: LifecycleAction) -> PendingCommand {
        let overlay = LifecycleOverlay::of(&action);
        let mut entry = self.command(thread, lifecycle_command(action));
        entry.overlay = overlay;
        entry
    }

    pub(super) fn prepare(&mut self, intent: Intent) -> Result<Next, PeerError> {
        // The open draft is frozen as it was before this intent changes it.
        self.state.freeze_open_draft();
        let next = self.prepare_intent(intent);
        self.state
            .settle_new_thread_drafts(super::owner::now_ms() as i64);
        next
    }

    fn prepare_intent(&mut self, intent: Intent) -> Result<Next, PeerError> {
        Ok(match intent {
            Intent::OpenThread { thread_id: id } => {
                self.select_thread(Some(thread_id(id)?));
                Next::Done
            }
            Intent::LeaveThread => {
                self.select_thread(None);
                Next::Done
            }
            Intent::NewThread { project_id } => {
                self.select_thread(None);
                self.state.selected_project = project_id;
                Next::Done
            }
            Intent::ShowArchived { open } => {
                self.show_archived(open);
                Next::Done
            }
            Intent::FilterProject { project_id } => {
                self.state.selected_project = project_id;
                Next::Done
            }
            Intent::Search { query } => {
                self.search(query);
                Next::Done
            }
            Intent::ReorderPinned {
                thread_id: moved,
                before_thread_id,
            } => self.reorder_pinned(moved, before_thread_id)?,
            Intent::EditDraft { text, base_text } => {
                let mut draft = self.state.current_draft();
                draft.text = match base_text {
                    Some(base) => merge_draft_text(base, text, draft.text.clone()),
                    None => text,
                };
                let key = self.state.draft_key();
                self.state.drafts.insert(key, draft);
                Next::Done
            }
            Intent::AttachFiles { draft_key, files } => {
                self.attach_files(draft_key, files)?;
                Next::Done
            }
            Intent::SelectComposerItem {
                text,
                cursor,
                item_id,
            } => self.select_composer_item(text, cursor, item_id)?,
            Intent::RemoveDraftContext { context_id } => {
                self.remove_draft_context(&context_id);
                Next::Done
            }
            Intent::AddTerminalContext {
                text,
                cursor,
                selection,
            } => self.add_terminal_context(text, cursor, &selection),
            Intent::AddThreadContexts {
                text,
                cursor,
                thread_ids,
            } => self.add_thread_contexts(text, cursor, &thread_ids),
            Intent::AttachTerminalOutput { output } => self.attach_terminal_output(&output)?,
            Intent::DiscardDraft { draft_key } => {
                self.discard_draft(draft_key);
                Next::Done
            }
            Intent::UndoThreadAction => self.undo_thread_action()?,
            Intent::SelectTrait {
                descriptor_id,
                choice,
            } => self.select_trait(&descriptor_id, &choice)?,
            Intent::ToggleTrait { descriptor_id, on } => self.toggle_trait(&descriptor_id, on)?,
            Intent::StashDraft => self.stash_draft()?,
            Intent::FinalizeStashImages { entry_id, images } => {
                self.finalize_stash_images(&entry_id, images);
                Next::Done
            }
            Intent::RestoreStash { entry_id } => self.restore_stash(&entry_id)?,
            Intent::DeleteStash { entry_id } => {
                self.state.stash.take(&entry_id);
                Next::Done
            }
            Intent::EditAnswer {
                request_id,
                question_id,
                edit,
            } => self.edit_answer(request_id, &question_id, edit)?,
            Intent::ShowQuestion { request_id, index } => {
                self.state
                    .question_drafts
                    .entry(request_id)
                    .or_default()
                    .question_index = index;
                Next::Done
            }
            Intent::SubmitAnswers { request_id } => self.submit_answers(request_id)?,
            Intent::MoveThread {
                thread_id: moved,
                section,
                destination,
            } => self.move_thread(&moved, section, &destination)?,
            Intent::DropThread {
                thread_id: id,
                plan,
            } => self.drop_thread(thread_id(id)?, plan)?,
            Intent::LimitRecovery {
                thread_id: id,
                action,
            } => self.limit_recovery(thread_id(id)?, action)?,
            Intent::DismissThreadError { dismiss_key } => {
                self.state.error_dismissals.dismiss(Some(&dismiss_key));
                self.state.error = None;
                Next::Done
            }
            Intent::SelectDiffScope { choice } => self.select_diff_scope(&choice)?,
            Intent::SelectDiffTurn { run_id, file_path } => {
                self.select_diff_turn(&run_id, file_path.as_deref())?
            }
            Intent::SelectDiffBaseRef { base_ref } => {
                self.select_diff_base_ref(base_ref.as_deref())?
            }
            Intent::SetDiffIgnoreWhitespace { ignore } => {
                self.state.preferences.diff_ignore_whitespace = ignore;
                if self.state.selected_thread.is_some() {
                    self.load_diff()?
                } else {
                    Next::Done
                }
            }
            Intent::LoadDiff => self.load_diff()?,
            Intent::OpenTerminal {
                thread_id: id,
                terminal_id,
                cols,
                rows,
            } => self.open_terminal(
                thread_id(id)?,
                terminal_id,
                None,
                op::TerminalSize { cols, rows },
                vec![],
            )?,
            Intent::NewTerminal {
                thread_id: id,
                cols,
                rows,
            } => self.new_terminal(
                thread_id(id)?,
                None,
                op::TerminalSize { cols, rows },
                vec![],
            )?,
            Intent::SplitTerminal {
                thread_id: id,
                terminal_id,
                cols,
                rows,
            } => self.new_terminal(
                thread_id(id)?,
                Some(&terminal_id),
                op::TerminalSize { cols, rows },
                vec![],
            )?,
            Intent::RunProjectScript {
                thread_id: id,
                script_id,
                cols,
                rows,
            } => self.run_project_script(
                thread_id(id)?,
                &script_id,
                op::TerminalSize { cols, rows },
            )?,
            Intent::WriteTerminal {
                thread_id: id,
                terminal_id,
                data,
            } => self.terminal_call(&thread_id(id)?, &terminal_id, |handle| {
                Call::WriteTerminal(op::TerminalWrite {
                    process_handle: handle,
                    data,
                })
            }),
            Intent::ResizeTerminal {
                thread_id: id,
                terminal_id,
                cols,
                rows,
            } => self.terminal_call(&thread_id(id)?, &terminal_id, |handle| {
                Call::ResizeTerminal(op::ResizeTerminal {
                    handle,
                    size: op::TerminalSize { cols, rows },
                })
            }),
            Intent::DetachTerminal {
                thread_id: id,
                terminal_id,
            } => self.terminal_call(&thread_id(id)?, &terminal_id, |handle| {
                Call::DetachTerminal(op::DetachTerminal { handle })
            }),
            Intent::CloseTerminal {
                thread_id: id,
                terminal_id,
            } => self.close_terminal(&thread_id(id)?, &terminal_id),
            Intent::UpdateComposerMenu {
                text,
                cursor,
                layout,
            } => {
                self.update_composer_menu(&text, cursor, layout);
                Next::Done
            }
            Intent::SearchDiffBaseRefs { query } => {
                let cwd = self.state.cwd();
                self.load_refs(cwd.clone(), crate::state::RefScope::Local, query.clone());
                self.load_refs(cwd, crate::state::RefScope::Remote, query);
                Next::Done
            }
            Intent::SearchNewThreadBranches { query } => {
                self.load_new_thread_branches(query);
                Next::Done
            }
            Intent::LoadMoreNewThreadBranches => {
                self.load_more_new_thread_branches();
                Next::Done
            }
            Intent::SetNewThreadWorkspace { mode } => self.set_new_thread_workspace(mode)?,
            Intent::SelectNewThreadBranch {
                branch,
                worktree_path,
            } => self.select_new_thread_branch(branch, worktree_path)?,
            Intent::CreateNewThreadBranch { name } => self.create_new_thread_branch(name)?,
            Intent::SetNewThreadStartFromOrigin { on } => {
                self.set_new_thread_start_from_origin(on)?
            }
            Intent::NewThreadOnBranch {
                project_id,
                branch,
                worktree_path,
            } => {
                self.select_thread(None);
                self.state.selected_project = Some(project_id);
                self.start_new_thread_on_branch(branch, worktree_path);
                self.load_new_thread_branches(String::new());
                Next::Done
            }
            Intent::RetryPreparation { run_id: run } => {
                let thread = self.selected()?;
                Next::Commands(vec![
                    self.command(thread, Command::RetryPrepared { run: run_id(run)? }),
                ])
            }
            Intent::WorkLocally => self.work_locally()?,
            Intent::CompactContext => self.compact_context()?,
            Intent::SetProjectIcon { project_id, path } => self.set_project_icon(project_id, path),
            Intent::ClearTerminal {
                thread_id: id,
                terminal_id,
            } => self.clear_terminal(thread_id(id)?, terminal_id),
            Intent::RestartTerminal {
                thread_id: id,
                terminal_id,
                cols,
                rows,
            } => {
                self.restart_terminal(thread_id(id)?, terminal_id, op::TerminalSize { cols, rows })?
            }
            Intent::SetFollowUpBehavior { behavior } => {
                self.state.follow_up = behavior;
                Next::Done
            }
            Intent::SetTimestampFormat { format } => {
                self.state.preferences.timestamp_format = format;
                Next::Done
            }
            Intent::SetWorkingSection { enabled } => {
                self.state.preferences.working_section = enabled;
                Next::Done
            }
            Intent::SetDefaultModel {
                instance_id,
                driver,
                model,
                options,
            } => {
                let draft = Draft {
                    instance_id,
                    driver,
                    model,
                    options,
                    ..self.state.default_draft.clone()
                };
                draft.selection().map_err(invalid)?;
                self.state.default_draft = draft;
                Next::Done
            }
            Intent::SetDefaultRuntimeMode { mode } => {
                self.state.default_draft.runtime_mode = mode;
                Next::Done
            }
            Intent::ToggleFavoriteModel { instance_id, model } => {
                self.toggle_favorite_model(&instance_id, &model);
                Next::Done
            }
            Intent::SetModelOrder {
                instance_id,
                models,
            } => {
                self.state
                    .preferences
                    .model_order
                    .insert(instance_id, models);
                Next::Done
            }
            Intent::DismissResumeCompaction { key } => {
                self.state.resume_compaction_dismissals.insert(key);
                Next::Done
            }
            Intent::UpsertKeybinding { rule, replace } => Next::call(
                Call::UpsertKeybinding(agent_protocol::keybindings::UpsertKeybinding {
                    rule: rule.into(),
                    replace: replace.map(Into::into),
                }),
                None,
            ),
            Intent::RemoveKeybinding { rule } => {
                Next::call(Call::RemoveKeybinding(rule.into()), None)
            }
            Intent::LoadConversationSettings => {
                Next::call(Call::ReadConversationSettings(m::Empty {}), None)
            }
            Intent::UpdateConversationSettings { scope, change } => {
                self.update_conversation_settings(&scope, &change)?
            }
            Intent::ResetProjectSettings { project_id } => {
                self.reset_project_settings(&project_id)?
            }
            Intent::UpdateProjectScripts {
                project_id,
                scripts,
            } => self.update_project_scripts(project_id, scripts),
            Intent::ScanSessions => self.scan_sessions(),
            Intent::SelectImportSessions { paths, checked } => {
                self.select_import_sessions(&paths, checked);
                Next::Done
            }
            Intent::CloseImport => {
                self.state.session_import = SessionImport::default();
                Next::Done
            }
            Intent::RetryAttachment { draft_key, id } => {
                let key = draft_key.unwrap_or_else(|| self.state.draft_key());
                self.begin_attachment(key, id)?;
                Next::Done
            }
            Intent::RemoveAttachment { draft_key, id } => {
                let key = draft_key.unwrap_or_else(|| self.state.draft_key());
                if let Some(draft) = self.state.drafts.get_mut(&key) {
                    draft.attachments.retain(|a| a.id != id);
                }
                Next::Done
            }
            Intent::Send { alternate } => self.send_draft(alternate)?,
            Intent::Stop => {
                let thread = self.selected()?;
                let command = self
                    .state
                    .thread_state(&thread)
                    .and_then(|state| interrupt_command(state, None))
                    .ok_or_else(|| invalid("No active work"))?;
                Next::Commands(vec![self.command(thread, command)])
            }
            Intent::StopSessions => {
                let thread = self.selected()?;
                let base = self.new_command_id();
                let commands = self
                    .state
                    .thread_state(&thread)
                    .map(|state| detach_commands(state, &base))
                    .unwrap_or_default();
                Next::Commands(
                    commands
                        .into_iter()
                        .map(|(id, command)| {
                            PendingCommand::new(
                                thread.clone(),
                                Request::Dispatch(Box::new(dispatch(thread.clone(), id, command))),
                                self.now(),
                            )
                        })
                        .collect(),
                )
            }
            Intent::DiscardPending { command_id } => {
                self.discard(&agent_domain::CommandId::new(command_id).map_err(invalid)?);
                Next::Done
            }
            Intent::Fork {
                source_thread_id,
                run_id: run,
            } => {
                let source = thread_id(source_thread_id)?;
                if self.state.context_pending(&source) {
                    return Ok(Next::Done);
                }
                let target = thread_id(new_id("thread"))?;
                let command =
                    fork_command(target, run_id(run)?, None, &self.options.creation_source);
                let mut entry = self.command(source, command);
                entry.navigate = true;
                Next::Commands(vec![entry])
            }
            Intent::MergeBack => {
                let thread = self.selected()?;
                if self.state.context_pending(&thread) {
                    return Ok(Next::Done);
                }
                let state = self
                    .state
                    .thread_state(&thread)
                    .ok_or_else(|| invalid("Thread is loading"))?;
                let run = latest_merge_back_run(state)
                    .ok_or_else(|| invalid("Wait for the latest run to finish"))?
                    .id
                    .clone();
                let parent = state
                    .thread
                    .as_ref()
                    .and_then(|t| t.parent.clone())
                    .ok_or_else(|| invalid("Thread is not a fork"))?;
                let mut entry = self.command(thread, merge_back_command(parent, run));
                entry.navigate = true;
                Next::Commands(vec![entry])
            }
            Intent::PlanFollowUp { new_thread } => self.plan_follow_up(new_thread)?,
            Intent::Rollback {
                checkpoint_id,
                restore_files,
            } => {
                let thread = self.selected()?;
                let checkpoint = self
                    .state
                    .thread_state(&thread)
                    .and_then(|state| {
                        state
                            .checkpoints
                            .iter()
                            .find(|c| c.id.as_str() == checkpoint_id)
                    })
                    .ok_or_else(|| invalid("Checkpoint unavailable"))?;
                let command = rollback_command(checkpoint, restore_files);
                Next::Commands(vec![self.command(thread, command)])
            }
            Intent::Thread {
                thread_id: id,
                action,
            } => {
                let action = match action {
                    ThreadAction::Pin => LifecycleAction::Pin { order: None },
                    ThreadAction::Unpin => LifecycleAction::Unpin,
                    ThreadAction::Settle => LifecycleAction::Settle,
                    ThreadAction::Unsettle => LifecycleAction::Unsettle,
                    ThreadAction::Snooze { until } => LifecycleAction::Snooze {
                        until: Timestamp::parse(&until).map_err(invalid)?,
                    },
                    ThreadAction::Unsnooze => LifecycleAction::Unsnooze,
                    ThreadAction::Rename { title } => LifecycleAction::Rename { title },
                    ThreadAction::RegenerateTitle => LifecycleAction::RegenerateTitle,
                    ThreadAction::MarkUnread => LifecycleAction::MarkUnread,
                    ThreadAction::AutoSettle { enabled } => LifecycleAction::AutoSettle { enabled },
                    ThreadAction::Archive => LifecycleAction::Archive,
                    ThreadAction::Unarchive => LifecycleAction::Unarchive,
                    ThreadAction::Delete => LifecycleAction::Delete,
                    ThreadAction::PinReorder { order_key } => {
                        LifecycleAction::ReorderPinned { order: order_key }
                    }
                    ThreadAction::ActiveReorder { order_key } => {
                        LifecycleAction::ReorderActive { order: order_key }
                    }
                    ThreadAction::Visit { at } => LifecycleAction::Visit {
                        at: Timestamp::from_millis(at).map_err(invalid)?,
                    },
                };
                Next::Commands(vec![self.lifecycle(thread_id(id)?, action)])
            }
            Intent::Queue { action } => self.queue(action)?,
            Intent::SetModel {
                instance_id,
                driver,
                model,
                options,
            } => {
                let mut draft = self.state.current_draft();
                draft.instance_id = instance_id;
                draft.driver = driver;
                draft.model = model;
                draft.options = options;
                let selection = draft.selection().map_err(invalid)?;
                self.state.default_draft = Draft {
                    text: String::new(),
                    attachments: vec![],
                    ..draft.clone()
                };
                let key = self.state.draft_key();
                self.state.drafts.insert(key, draft);
                self.thread_command(select_model_command(selection))
            }
            Intent::SetRuntimeMode { mode } => {
                self.update_draft(|draft| draft.runtime_mode = mode);
                self.thread_command(Command::RuntimeMode { mode })
            }
            Intent::SetInteractionMode { mode } => {
                self.update_draft(|draft| draft.interaction_mode = mode);
                self.thread_command(Command::InteractionMode { mode })
            }
            Intent::RespondApproval {
                request_id,
                decision,
            } => {
                let thread = self.selected()?;
                let request = RuntimeRequestId::new(request_id).map_err(invalid)?;
                let command = approval_command(request, approval_decision(&decision)?);
                Next::Commands(vec![self.command(thread, command)])
            }
            Intent::DismissInput { request_id } => {
                let thread = self.selected()?;
                let command = Command::DismissQuestion {
                    request: RuntimeRequestId::new(request_id).map_err(invalid)?,
                };
                Next::Commands(vec![self.command(thread, command)])
            }
            Intent::LoadEarlier => {
                self.load_earlier()?;
                Next::Done
            }
            Intent::LoadItemDetail { item_id } => {
                self.load_detail(agent_domain::TurnItemId::new(item_id).map_err(invalid)?)?;
                Next::Done
            }
            Intent::CancelSetup => Next::call(
                Call::CancelSetup(c::CancelSetup {
                    thread_id: self.selected()?,
                }),
                None,
            ),
            Intent::Refresh => {
                self.refresh();
                self.app_became_active();
                Next::Done
            }
            other => self.peripheral(other)?,
        })
    }

    fn update_draft(&mut self, change: impl FnOnce(&mut Draft)) {
        let mut draft = self.state.current_draft();
        change(&mut draft);
        let key = self.state.draft_key();
        self.state.drafts.insert(key, draft);
    }

    /// A thread setting changes the open thread; a new-thread draft keeps it.
    fn thread_command(&self, command: Command) -> Next {
        match &self.state.selected_thread {
            Some(thread) => Next::Commands(vec![self.command(thread.clone(), command)]),
            None => Next::Done,
        }
    }

    fn reorder_pinned(&mut self, moved: String, before: Option<String>) -> Result<Next, PeerError> {
        let shell = self
            .state
            .shell_view()
            .ok_or_else(|| invalid("Pinned thread is unavailable"))?
            .into_owned();
        let mut pinned: Vec<_> = shell
            .threads
            .iter()
            .filter(|row| row.pinned_at.is_some())
            .collect();
        sort_pinned_by_order(&mut pinned);
        let mut ids: Vec<String> = pinned.iter().map(|row| row.id.to_string()).collect();
        let from = ids
            .iter()
            .position(|id| id == &moved)
            .ok_or_else(|| invalid("Pinned thread is unavailable"))?;
        if before.as_ref() == Some(&moved) {
            return Ok(Next::Done);
        }
        ids.remove(from);
        let to = match before {
            Some(before) => ids
                .iter()
                .position(|id| id == &before)
                .ok_or_else(|| invalid("Pinned list changed; try again"))?,
            None => ids.len(),
        };
        ids.insert(to, moved.clone());
        let keys = pinned
            .iter()
            .map(|row| (row.id.to_string(), row.pin_order.clone()))
            .collect();
        let entries = crate::ordering::reorder(&ids, &keys, &moved)
            .into_iter()
            .map(|(id, order)| {
                Ok(self.lifecycle(thread_id(id)?, LifecycleAction::ReorderPinned { order }))
            })
            .collect::<Result<_, PeerError>>()?;
        Ok(Next::Commands(entries))
    }

    fn send_draft(&mut self, alternate: bool) -> Result<Next, PeerError> {
        if self.state.editing_run.is_some() {
            return self.queue(QueueAction::SaveEdit);
        }
        let draft = self.state.current_draft();
        let slash = draft.text.trim().to_ascii_lowercase();
        if matches!(slash.as_str(), "/plan" | "/default") && draft.attachments.is_empty() {
            let mode = if slash == "/plan" {
                InteractionMode::Plan
            } else {
                InteractionMode::Default
            };
            self.update_draft(|draft| {
                draft.text.clear();
                draft.interaction_mode = mode;
            });
            return Ok(self.thread_command(Command::InteractionMode { mode }));
        }
        if self.options.creation_source == "desktop"
            && !alternate
            && actionable_plan(&self.state).is_some()
        {
            return self.plan_follow_up(false);
        }
        if draft.is_empty() {
            return Err(invalid("Enter a message"));
        }
        let selection = draft.selection().map_err(invalid)?;
        let attachments = draft.attachment_refs().map_err(invalid)?;
        let names: Vec<&str> = attachments.iter().map(|a| a.name.as_str()).collect();
        let title_seed = thread_title_seed(&draft.text, &names, &[]);
        let message = TurnMessage {
            id: MessageId::new(new_id("message")).map_err(invalid)?,
            text: draft.text.clone(),
            attachments: attachments.clone(),
            context: draft.context.clone(),
        };
        let key = self.state.draft_key();
        let restore = Restore {
            draft_key: key.clone(),
            text: draft.text.clone(),
            attachments,
            context: draft.context.clone(),
        };
        let mut entry = match self.state.selected_thread.clone() {
            Some(thread) => {
                let running = self
                    .state
                    .thread_state(&thread)
                    .is_some_and(|state| state.active_run().is_some());
                let mode =
                    resolve_composer_dispatch_mode(running, alternate, Some(self.state.follow_up));
                let command = send_command(StartTurn {
                    message,
                    selection: Some(selection),
                    title_seed: Some(title_seed),
                    source_plan: None,
                    dispatch: mode.into(),
                    continuation: None,
                    creation_source: self.options.creation_source.clone(),
                });
                self.command(thread, command)
            }
            None => {
                let workspace = crate::view::new_thread::new_thread_launch_workspace(&self.state)
                    .map_err(invalid)?;
                let thread = thread_id(new_id("thread"))?;
                let launch = launch(LaunchThread {
                    command_id: self.new_command_id(),
                    thread: Some(thread.clone()),
                    project: self
                        .state
                        .selected_project
                        .clone()
                        .unwrap_or_else(|| CHATS_PROJECT.into()),
                    title: title_seed.clone(),
                    title_seed: Some(title_seed),
                    selection,
                    runtime_mode: draft.runtime_mode,
                    interaction_mode: draft.interaction_mode,
                    workspace,
                    message: Some(message),
                    creation_source: self.options.creation_source.clone(),
                });
                let mut entry =
                    PendingCommand::new(thread, Request::Launch(Box::new(launch)), self.now());
                entry.navigate = true;
                entry
            }
        };
        entry.restore = Some(restore);
        let mut cleared = draft;
        cleared.text.clear();
        cleared.attachments.clear();
        cleared.context = None;
        self.state.drafts.insert(key, cleared);
        Ok(Next::Commands(vec![entry]))
    }

    fn compact_context(&mut self) -> Result<Next, PeerError> {
        let thread = self.selected()?;
        let selection = self.state.current_draft().selection().map_err(invalid)?;
        let command = send_command(StartTurn {
            message: TurnMessage {
                id: MessageId::new(new_id("message")).map_err(invalid)?,
                text: "/compact".into(),
                attachments: vec![],
                context: None,
            },
            selection: Some(selection),
            title_seed: None,
            source_plan: None,
            dispatch: resolve_composer_dispatch_mode(false, false, Some(self.state.follow_up))
                .into(),
            continuation: None,
            creation_source: self.options.creation_source.clone(),
        });
        Ok(Next::Commands(vec![self.command(thread, command)]))
    }

    fn plan_follow_up(&mut self, new_thread: bool) -> Result<Next, PeerError> {
        let thread = self.selected()?;
        let state = self
            .state
            .thread_state(&thread)
            .ok_or_else(|| invalid("Thread not loaded"))?;
        let plan = actionable_plan(&self.state).ok_or_else(|| invalid("No actionable plan"))?;
        let markdown = crate::view::plan::proposed_plan_markdown(state, plan);
        let plan_ref = PlanRef {
            thread: thread.clone(),
            plan: plan.id.clone(),
        };
        let current = state.thread.as_ref().expect("thread record");
        let (project, workspace, mode_now) = (
            current.project.clone(),
            current.workspace.clone(),
            current.interaction_mode,
        );
        let draft = self.state.current_draft();
        let selection = draft.selection().map_err(invalid)?;
        let message = |text: String| -> Result<TurnMessage, PeerError> {
            Ok(TurnMessage {
                id: MessageId::new(new_id("message")).map_err(invalid)?,
                text,
                attachments: vec![],
                context: None,
            })
        };
        if new_thread {
            let child = thread_id(new_id("thread"))?;
            let launch = launch(LaunchThread {
                command_id: self.new_command_id(),
                thread: Some(child.clone()),
                project,
                title: plan_implementation_thread_title(&markdown),
                title_seed: None,
                selection,
                runtime_mode: self.state.default_draft.runtime_mode,
                interaction_mode: InteractionMode::Default,
                workspace: WorkspaceChoice::Local {
                    branch: workspace.as_ref().and_then(|w| w.branch.clone()),
                    worktree_path: workspace.and_then(|w| w.worktree_path),
                },
                message: Some(message(plan_implementation_prompt(&markdown))?),
                creation_source: self.options.creation_source.clone(),
            });
            let mut entry =
                PendingCommand::new(child, Request::Launch(Box::new(launch)), self.now());
            entry.navigate = true;
            return Ok(Next::Commands(vec![entry]));
        }
        let (text, mode) = plan_follow_up(&draft.text, &markdown);
        let implementing = mode == InteractionMode::Default;
        let mut entries = vec![];
        if mode != mode_now {
            entries.push(self.command(thread.clone(), Command::InteractionMode { mode }));
            self.update_draft(|draft| draft.interaction_mode = mode);
        }
        let mut send = self.command(
            thread.clone(),
            send_command(StartTurn {
                message: message(text)?,
                selection: Some(selection),
                title_seed: None,
                source_plan: implementing.then_some(plan_ref),
                dispatch: TurnDispatch::Auto,
                continuation: None,
                creation_source: self.options.creation_source.clone(),
            }),
        );
        if !implementing {
            let key = self.state.draft_key();
            send.restore = Some(Restore {
                draft_key: key.clone(),
                text: draft.text.clone(),
                attachments: vec![],
                context: None,
            });
            self.update_draft(|draft| draft.text.clear());
        }
        entries.push(send);
        Ok(Next::Commands(entries))
    }

    fn queue(&mut self, action: QueueAction) -> Result<Next, PeerError> {
        let thread = self.selected()?;
        Ok(match action {
            QueueAction::Resume => Next::Commands(vec![self.command(thread, Command::ResumeQueue)]),
            QueueAction::Cancel { run_id: run } => Next::Commands(vec![
                self.command(thread, Command::CancelQueued { run: run_id(run)? }),
            ]),
            QueueAction::Steer { run_id: run } => {
                let active = self
                    .state
                    .thread_state(&thread)
                    .and_then(State::active_run)
                    .map(|run| run.id.clone())
                    .ok_or_else(|| invalid("No active run"))?;
                Next::Commands(vec![self.command(
                    thread,
                    Command::PromoteToSteer {
                        queued: run_id(run)?,
                        active,
                    },
                )])
            }
            QueueAction::Edit { run_id: run } => {
                let run = run_id(run)?;
                let queued = self
                    .state
                    .thread_state(&thread)
                    .map(queue_workflow)
                    .and_then(|workflow| workflow.queued.into_iter().find(|q| q.run == run))
                    .ok_or_else(|| invalid("Queued run is unavailable"))?;
                let mut draft = self.state.current_draft();
                draft.text = queued.text;
                draft.attachments = queued
                    .attachments
                    .iter()
                    .map(DraftAttachment::from_remote)
                    .collect();
                draft.context = queued.context;
                self.state.editing_run = Some(run);
                let key = self.state.draft_key();
                self.state.drafts.insert(key, draft);
                Next::Done
            }
            QueueAction::SaveEdit => {
                let run = self
                    .state
                    .editing_run
                    .clone()
                    .ok_or_else(|| invalid("No queued message is being edited"))?;
                let draft = self.state.current_draft();
                let attachments = draft.attachment_refs().map_err(invalid)?;
                let mut entry = self.command(
                    thread.clone(),
                    Command::EditQueued {
                        run,
                        text: draft.text.clone(),
                        attachments: Some(attachments.clone()),
                        context: draft.context.clone(),
                    },
                );
                entry.restore = Some(Restore {
                    draft_key: thread.to_string(),
                    text: draft.text.clone(),
                    attachments,
                    context: draft.context.clone(),
                });
                let key = self.state.draft_key();
                self.state.drafts.remove(&key);
                self.state.editing_run = None;
                Next::Commands(vec![entry])
            }
            QueueAction::CancelEdit => {
                let key = self.state.draft_key();
                self.state.drafts.remove(&key);
                self.state.editing_run = None;
                Next::Done
            }
            QueueAction::Move {
                run_id: run,
                before_run_id,
            } => Next::Commands(vec![self.command(
                thread,
                Command::ReorderQueued {
                    run: run_id(run)?,
                    before: before_run_id.map(run_id).transpose()?,
                },
            )]),
        })
    }

    /// Admits the picked files by the reference rules, then uploads the accepted ones.
    /// Images over the size limit must be downscaled by the client first.
    fn attach_files(&mut self, key: String, files: Vec<LocalFile>) -> Result<(), PeerError> {
        if key != self.state.draft_key()
            && !self.state.drafts.contains_key(&key)
            && !key.starts_with("answer:")
        {
            return Err(invalid("The attachment draft is no longer available"));
        }
        let mut draft = self.state.drafts.get(&key).cloned().unwrap_or_else(|| {
            if key.starts_with("answer:") {
                Draft::default()
            } else {
                self.state.current_draft()
            }
        });
        let mut candidates = vec![];
        let mut readable = vec![];
        let mut error = None;
        for file in files {
            match std::fs::metadata(&file.path) {
                Ok(metadata) if metadata.is_file() => {
                    candidates.push(AttachmentCandidate {
                        name: file.name.clone(),
                        mime_type: file.mime_type.to_ascii_lowercase(),
                        size_bytes: metadata.len(),
                    });
                    readable.push(file);
                }
                _ => error = Some(format!("'{}' is empty or could not be read.", file.name)),
            }
        }
        let admission = admit_attachments(&draft.attachments, &candidates);
        let mut added = vec![];
        for admitted in admission.accepted {
            let index = admitted.index as usize;
            if admitted.needs_compression {
                error = Some(image_preparation_error(&admitted.name, false));
                continue;
            }
            let id = new_id("attachment");
            draft.attachments.push(DraftAttachment {
                id: id.clone(),
                remote_id: None,
                name: admitted.name,
                kind: match admitted.kind {
                    AttachmentFileKind::Image => "image",
                    _ => "file",
                }
                .into(),
                mime_type: admitted.mime_type,
                size_bytes: candidates[index].size_bytes,
                local_path: readable[index].path.clone(),
                status: "failed".into(),
                error: Some("Connect to upload".into()),
            });
            added.push(id);
        }
        self.state.drafts.insert(key.clone(), draft);
        if self.state.connected {
            for id in added {
                self.begin_attachment(key.clone(), id)?;
            }
        }
        match admission.error.or(error) {
            Some(error) => Err(invalid(error)),
            None => Ok(()),
        }
    }

    pub(super) fn begin_attachment(&mut self, key: String, id: String) -> Result<(), PeerError> {
        let sender = self.sender.clone();
        let epoch = self.epoch;
        let attachment = self
            .state
            .drafts
            .get_mut(&key)
            .and_then(|draft| draft.attachments.iter_mut().find(|a| a.id == id))
            .ok_or_else(|| invalid("Attachment is unavailable"))?;
        let (source, name, mime) = (
            attachment.local_path.clone(),
            attachment.name.clone(),
            attachment.mime_type.clone(),
        );
        let network = self
            .network
            .as_mut()
            .filter(|_| self.state.connected)
            .ok_or_else(|| invalid("Connect to the Host to upload attachments"))?;
        let (peer, session) = (network.peer.clone(), network.session.clone());
        let (task_key, task_id) = (key.clone(), id.clone());
        network.spawn(async move {
            let (key, id) = (task_key, task_id);
            let result = agent_transport::transfers::upload_attachment(
                &peer,
                || async { session.open_stream().await.map_err(std::io::Error::other) },
                std::path::Path::new(&source),
                &name,
                &mime,
            )
            .await
            .map_err(invalid)
            .and_then(|uploaded| {
                let remote = uploaded
                    .attachment
                    .ok_or_else(|| invalid("Host did not return an attachment"))?;
                Ok(agent_domain::Attachment {
                    path: String::new(),
                    ..remote
                })
            });
            let _ = sender
                .send(Event::AttachmentFinished(epoch, key, id, result))
                .await;
        });
        let attachment = self
            .state
            .drafts
            .get_mut(&key)
            .and_then(|draft| draft.attachments.iter_mut().find(|a| a.id == id))
            .expect("attachment checked above");
        attachment.status = "uploading".into();
        attachment.error = None;
        Ok(())
    }

    pub(super) fn attachment_finished(
        &mut self,
        key: String,
        id: String,
        result: Result<agent_domain::Attachment, PeerError>,
    ) {
        let Some(attachment) = self
            .state
            .drafts
            .get_mut(&key)
            .and_then(|draft| draft.attachments.iter_mut().find(|a| a.id == id))
        else {
            return;
        };
        match result {
            Ok(remote) => {
                attachment.remote_id = Some(remote.id);
                attachment.kind = match remote.kind {
                    agent_domain::AttachmentKind::Image => "image",
                    agent_domain::AttachmentKind::File => "file",
                }
                .into();
                attachment.mime_type = remote.mime_type;
                attachment.size_bytes = remote.size;
                attachment.status = "ready".into();
                attachment.error = None;
            }
            Err(error) => {
                attachment.status = "failed".into();
                attachment.error = Some(crate::presentation::error::error_message(
                    &error.to_string(),
                ));
            }
        }
    }

    fn peripheral(&mut self, intent: Intent) -> Result<Next, PeerError> {
        Ok(match intent {
            Intent::Transcribe {
                draft_key,
                preparation,
                audio,
            } => {
                let draft = self
                    .state
                    .drafts
                    .get(&draft_key)
                    .cloned()
                    .unwrap_or_else(|| self.state.current_draft());
                Next::call(
                    Call::Transcribe(op::Transcribe { preparation, audio }),
                    Some((draft_key, draft)),
                )
            }
            Intent::ListFiles { path } => {
                self.state.workspace.requested_directory = Some(path.clone());
                Next::call(Call::ListFiles(op::ListFiles { path }), None)
            }
            Intent::ReadFile {
                path,
                discard_draft,
            } => {
                self.state.workspace.requested_file = Some(path.clone());
                if discard_draft {
                    self.state.workspace.file_drafts.remove(&path);
                }
                Next::call(Call::ReadFile(op::ListFiles { path }), None)
            }
            Intent::EditFile { path, text } => {
                let file = self
                    .state
                    .workspace
                    .file
                    .as_ref()
                    .filter(|file| file.path == path)
                    .ok_or_else(|| invalid("Open the file before editing"))?;
                let revision = file.revision.clone();
                self.state
                    .workspace
                    .file_drafts
                    .entry(path)
                    .and_modify(|draft| Arc::make_mut(draft).text = text.clone())
                    .or_insert_with(|| Arc::new(FileDraft { text, revision }));
                Next::Done
            }
            Intent::SaveFile { path } => {
                let draft = self
                    .state
                    .workspace
                    .file_drafts
                    .get(&path)
                    .cloned()
                    .ok_or_else(|| invalid("File has no edits"))?;
                Next::call(
                    Call::WriteFile(op::WriteFile {
                        path,
                        revision: draft.revision.clone(),
                        text: draft.text.clone(),
                    }),
                    None,
                )
            }
            Intent::ReviewWorkspace { cwd } => {
                self.state.workspace.diff_request = None;
                self.state.workspace.review = None;
                Next::call(Call::ReviewWorkspace(op::ReviewWorkspace { cwd }), None)
            }
            Intent::ReadTurnDiff {
                from_run_ordinal,
                to_run_ordinal,
                ignore_whitespace,
            } => {
                let request = c::GetTurnDiff {
                    thread_id: self.selected()?,
                    from_run_ordinal,
                    to_run_ordinal,
                    ignore_whitespace: Some(ignore_whitespace),
                };
                self.state.workspace.review = None;
                self.state.workspace.diff_request = Some(request.clone());
                Next::call(Call::TurnDiff(request), None)
            }
            Intent::LoadWorktreeSettings => {
                Next::call(Call::ReadWorktreeSettings(m::Empty {}), None)
            }
            Intent::SaveWorktreeSettings { settings } => {
                Next::call(Call::UpdateWorktreeSettings(settings), None)
            }
            Intent::ListWorktrees => Next::call(Call::ListWorktrees(m::Empty {}), None),
            Intent::RemoveWorktree { path } => {
                Next::call(Call::RemoveWorktree(op::RemoveWorktree { path }), None)
            }
            Intent::LoadAccounts => Next::call(Call::ListAccounts(m::Empty {}), None),
            Intent::SelectAccount { provider, id } => Next::call(
                Call::SelectAccount(op::SelectAccount { provider, id }),
                None,
            ),
            Intent::StartLogin { provider } => Next::call(
                Call::StartAccountLogin(op::StartAccountLogin { provider }),
                None,
            ),
            Intent::CompleteLogin { provider, id, code } => Next::call(
                Call::SubmitAccountLogin(op::SubmitAccountLogin { provider, id, code }),
                None,
            ),
            Intent::CancelLogin { provider, id } => Next::call(
                Call::CancelAccountLogin(op::CancelAccountLogin { provider, id }),
                None,
            ),
            Intent::DeleteAccount { provider, id } => Next::call(
                Call::LogoutAccount(op::LogoutAccount { provider, id }),
                None,
            ),
            Intent::LoadHostStatus => Next::call(Call::HostStatus(m::Empty {}), None),
            Intent::LoadRemoteHosts => Next::call(Call::ListRemotes(m::Empty {}), None),
            Intent::LoadHostManagement => {
                self.job(Call::HostStatus(m::Empty {}), None, None);
                Next::call(Call::ListRemotes(m::Empty {}), None)
            }
            Intent::RemoveRemoteHost { id } => {
                Next::call(Call::RemoveRemote(op::RemoveRemoteHost { id }), None)
            }
            Intent::CreateInvitation => Next::call(Call::Invite(m::Empty {}), None),
            Intent::RevokeDevice { id } => Next::call(Call::Revoke(op::RevokeDevice { id }), None),
            Intent::AddProject { path } => {
                Next::call(Call::AddProject(op::AddProject { cwd: path }), None)
            }
            _ => unreachable!("conversation intents are prepared above"),
        })
    }
}
