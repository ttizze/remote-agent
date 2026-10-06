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
};
use agent_domain::{
    Answer, Answers, ApprovalDecision, Command, InteractionMode, ItemKind, MessageId, Plan,
    PlanKind, PlanRef, RequestBody, RunId, RuntimeRequestId, State, ThreadId, Timestamp,
};
use agent_protocol::{conversation as c, models as m, operations as op};
use std::{collections::BTreeMap, sync::Arc};

pub(super) enum Next {
    Done,
    /// Outbox entries in order; the last one resolves the intent.
    Commands(Vec<PendingCommand>),
    /// A request whose reply resolves the intent.
    Call(Box<Call>, Option<(String, Draft)>),
}
impl Next {
    fn call(call: Call, sent: Option<(String, Draft)>) -> Self {
        Self::Call(Box::new(call), sent)
    }
}

fn invalid(error: impl std::fmt::Display) -> PeerError {
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

/// The proposed plan a follow-up implements or refines.
pub(super) fn actionable_plan(state: &State) -> Option<&Plan> {
    let thread = state.thread.as_ref()?;
    if thread.interaction_mode != InteractionMode::Plan || state.active_run().is_some() {
        return None;
    }
    state
        .plans
        .iter()
        .rev()
        .find(|plan| plan.kind == PlanKind::Proposed && plan.implemented_by.is_none())
}

/// Bounded snapshots keep the plan text on its item only.
fn plan_markdown(state: &State, plan: &Plan) -> String {
    if !plan.markdown.is_empty() {
        return plan.markdown.clone();
    }
    state
        .items
        .iter()
        .find(|item| matches!(&item.kind, ItemKind::ProposedPlan { plan: id } if id == &plan.id))
        .map(|item| item.text.clone())
        .unwrap_or_default()
}

impl Owner {
    pub(super) fn intent(&mut self, intent: Intent, complete: Waiter) {
        if let Intent::PairRemoteHost { invitation, name } = intent {
            self.pair(invitation, name, complete);
            return;
        }
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
            Ok(Next::Commands(mut entries)) => {
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
            Ok(Next::Call(call, sent)) => self.job(*call, Some(complete), sent),
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

    fn command(&self, thread: ThreadId, command: Command) -> PendingCommand {
        self.pending(thread, command)
    }

    fn lifecycle(&self, thread: ThreadId, action: LifecycleAction) -> PendingCommand {
        let overlay = LifecycleOverlay::of(&action);
        let mut entry = self.command(thread, lifecycle_command(action));
        entry.overlay = overlay;
        entry
    }

    pub(super) fn prepare(&mut self, intent: Intent) -> Result<Next, PeerError> {
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
                self.state.search = query.clone();
                self.state.search_matches.clear();
                let query = query.trim();
                if self.connected() && (2..=200).contains(&query.encode_utf16().count()) {
                    Next::call(
                        Call::Search(c::Search {
                            query: query.into(),
                            limit: Some(50),
                        }),
                        None,
                    )
                } else {
                    Next::Done
                }
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
            Intent::AttachFile {
                path,
                name,
                mime_type,
                draft_key,
            } => {
                self.attach_file(path, name, mime_type, draft_key)?;
                Next::Done
            }
            Intent::RetryAttachment { id } => {
                self.begin_attachment(self.state.draft_key(), id)?;
                Next::Done
            }
            Intent::RemoveAttachment { id } => {
                let key = self.state.draft_key();
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
            Intent::RespondQuestions {
                request_id,
                answers,
            } => {
                let thread = self.selected()?;
                let request = RuntimeRequestId::new(request_id).map_err(invalid)?;
                let questions = self
                    .state
                    .thread_state(&thread)
                    .and_then(|state| state.requests.iter().find(|r| r.id == request))
                    .and_then(|r| match &r.body {
                        RequestBody::Questions { questions } => Some(questions.clone()),
                        RequestBody::Approval { .. } => None,
                    })
                    .ok_or_else(|| invalid("Question is unavailable"))?;
                let answers: Answers = answers
                    .into_iter()
                    .map(|answer| {
                        let multiple = questions
                            .iter()
                            .any(|q| q.id == answer.question_id && q.multiple);
                        let value = if multiple {
                            Answer::Choices(answer.values)
                        } else {
                            Answer::Text(answer.values.into_iter().next().unwrap_or_default())
                        };
                        (answer.question_id, value)
                    })
                    .collect();
                let command = answers_command(request, answers, BTreeMap::new());
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
            && self
                .state
                .selected_state()
                .and_then(actionable_plan)
                .is_some()
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
            context: None,
        };
        let key = self.state.draft_key();
        let restore = Restore {
            draft_key: key.clone(),
            text: draft.text.clone(),
            attachments,
            context: None,
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
                    workspace: WorkspaceChoice::Local {
                        branch: None,
                        worktree_path: None,
                    },
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
        self.state.drafts.insert(key, cleared);
        Ok(Next::Commands(vec![entry]))
    }

    fn plan_follow_up(&mut self, new_thread: bool) -> Result<Next, PeerError> {
        let thread = self.selected()?;
        let state = self
            .state
            .thread_state(&thread)
            .ok_or_else(|| invalid("Thread not loaded"))?;
        let plan = actionable_plan(state).ok_or_else(|| invalid("No actionable plan"))?;
        let markdown = plan_markdown(state, plan);
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
                        context: None,
                    },
                );
                entry.restore = Some(Restore {
                    draft_key: thread.to_string(),
                    text: draft.text.clone(),
                    attachments,
                    context: None,
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
            QueueAction::Reorder { run_ids } => {
                let ordered = run_ids
                    .into_iter()
                    .map(run_id)
                    .collect::<Result<Vec<_>, _>>()?;
                let entries = ordered
                    .iter()
                    .enumerate()
                    .rev()
                    .map(|(index, run)| {
                        self.command(
                            thread.clone(),
                            Command::ReorderQueued {
                                run: run.clone(),
                                before: ordered.get(index + 1).cloned(),
                            },
                        )
                    })
                    .collect();
                Next::Commands(entries)
            }
        })
    }

    fn attach_file(
        &mut self,
        path: String,
        name: String,
        mime_type: String,
        key: String,
    ) -> Result<(), PeerError> {
        let metadata = std::fs::metadata(&path).map_err(invalid)?;
        if !metadata.is_file() {
            return Err(invalid("Choose a regular file"));
        }
        if key != self.state.draft_key() && !self.state.drafts.contains_key(&key) {
            return Err(invalid("The attachment draft is no longer available"));
        }
        let mime_type = mime_type.to_ascii_lowercase();
        let id = new_id("attachment");
        let attachment = DraftAttachment {
            id: id.clone(),
            remote_id: None,
            name,
            kind: if native_image(&mime_type) {
                "image"
            } else {
                "file"
            }
            .into(),
            mime_type,
            size_bytes: metadata.len(),
            local_path: path,
            status: "failed".into(),
            error: Some("Connect to upload".into()),
        };
        let mut draft = self
            .state
            .drafts
            .get(&key)
            .cloned()
            .unwrap_or_else(|| self.state.current_draft());
        let mut references: Vec<_> = draft
            .attachments
            .iter()
            .map(DraftAttachment::metadata)
            .collect();
        references.push(attachment.metadata());
        if references.len() > 100 {
            return Err(invalid("You can attach up to 100 files per message."));
        }
        draft.attachments.push(attachment);
        self.state.drafts.insert(key.clone(), draft);
        if self.state.connected {
            self.begin_attachment(key, id)?;
        }
        Ok(())
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
                    kind: if native_image(&remote.mime_type) {
                        agent_domain::AttachmentKind::Image
                    } else {
                        agent_domain::AttachmentKind::File
                    },
                    source: None,
                    id: remote.id,
                    name: remote.name,
                    mime_type: remote.mime_type,
                    path: String::new(),
                    size: remote.size_bytes,
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
            Intent::StartTerminal {
                handle,
                cwd,
                cols,
                rows,
            } => {
                let size = op::TerminalSize { cols, rows };
                let previous = self.state.terminals.get(&handle);
                let terminal = Terminal {
                    cwd: cwd.clone(),
                    size,
                    phase: TerminalPhase::Starting,
                    output: previous.map(|t| t.output.clone()).unwrap_or_default(),
                    sequence: previous.map_or(0, |t| t.sequence),
                    output_bytes: previous.map_or(0, |t| t.output_bytes),
                };
                self.state.terminals.insert(handle.clone(), terminal);
                Next::call(
                    Call::StartTerminal(op::StartTerminal { handle, cwd, size }),
                    None,
                )
            }
            Intent::ResizeTerminal { handle, cols, rows } => Next::call(
                Call::ResizeTerminal(op::ResizeTerminal {
                    handle,
                    size: op::TerminalSize { cols, rows },
                }),
                None,
            ),
            Intent::WriteTerminal { handle, data } => Next::call(
                Call::WriteTerminal(op::TerminalWrite {
                    process_handle: handle,
                    data,
                }),
                None,
            ),
            Intent::DetachTerminal { handle } => {
                Next::call(Call::DetachTerminal(op::DetachTerminal { handle }), None)
            }
            Intent::KillTerminal { handle } => Next::call(
                Call::KillTerminal(op::TerminalKill {
                    process_handle: handle,
                }),
                None,
            ),
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
            Intent::RegisterProject { path } => {
                Next::call(Call::AddProject(op::AddProject { cwd: path }), None)
            }
            _ => unreachable!("conversation intents are prepared above"),
        })
    }
}
