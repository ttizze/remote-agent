//! Intents over device state the views read: composer menus and traits, the
//! prompt stash, answer drafts, list holds, the diff panel and preferences.
use super::{
    Outcome,
    intents::{Next, invalid},
    owner::{Owner, new_id, now_ms},
};
use crate::{
    commands::build::{LifecycleAction, MetadataChange, metadata_command, select_model_command},
    peer::PeerError,
    state::*,
    view::{
        checkpoints::{DiffScopeChoice, checkpoint_summaries, diff_panel},
        composer::{
            chips::{format_context_reference, insert_inline_context_references},
            commands::{
                ComposerTrigger, TOO_MANY_CONTEXT_ITEMS, ThreadContextAttachment,
                detect_composer_trigger, resolve_composer_command_selection,
            },
            menu::composer_menu_items,
            stash::{StashImages, evicted_entry_warning, new_stash_entry, restore_stash_entry},
            terminal_context::{
                TerminalContextSelection, is_same_terminal_range,
                normalize_terminal_context_selection, terminal_context_record,
                terminal_context_reference,
            },
        },
        models::{
            catalog,
            ordering::toggle_favorite,
            staging::remember_model_options,
            traits::{TraitChange, select_trait, toggle_trait},
        },
        requests::{
            answer_drafts, carry_displaced_custom_answer_into_prompt, question_answers,
            set_question_custom_answer, toggle_question_option,
        },
        sidebar::SidebarThreadDropPlan,
        terminals::{TerminalOutputContext, append_context_reference},
        thread_list::{ordered_section, queued_threads},
        thread_order::{
            DropSection, MoveDestination, OrderSection, PendingThreadOrder, ThreadMovePlanner,
            thread_drop_lifecycle,
        },
        thread_summary::ThreadSummary,
        timeline::banners::{
            RecoveryAction, RecoveryToggle, toggle_limit_recovery, usage_limit_recovery,
        },
    },
};
use agent_domain::{
    Attachment, AttachmentKind, Command, Json, Question, RequestBody, RuntimeRequestId, ThreadId,
};
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};

fn context_id(record: &Json) -> Option<&str> {
    record.0.get("contextId").and_then(Value::as_str)
}

fn context_records(draft: &Draft) -> &[Json] {
    draft
        .context
        .as_ref()
        .map_or(&[][..], |context| context.records.as_slice())
}

fn push_context_record(draft: &mut Draft, record: Value) {
    draft
        .context
        .get_or_insert_with(|| agent_domain::MessageContext {
            version: 1,
            records: vec![],
        })
        .records
        .push(Json(record));
}

impl Owner {
    fn set_draft(&mut self, draft: Draft) {
        let key = self.state.draft_key();
        self.state.drafts.insert(key, draft);
    }

    pub(super) fn select_composer_item(
        &mut self,
        text: String,
        cursor: u32,
        item_id: String,
    ) -> Result<Next, PeerError> {
        let trigger: ComposerTrigger =
            detect_composer_trigger(&text, cursor).ok_or_else(|| invalid("Nothing to complete"))?;
        let item = composer_menu_items(&self.state, &trigger)
            .into_iter()
            .find(|item| item.id == item_id)
            .ok_or_else(|| invalid("That item is no longer offered"))?;
        let mut draft = self.state.current_draft();
        let context_ids: Vec<String> = draft
            .context
            .iter()
            .flat_map(|context| &context.records)
            .filter_map(|record| context_id(record).map(str::to_owned))
            .collect();
        let selection =
            resolve_composer_command_selection(&text, &trigger, &item, true, &context_ids)
                .map_err(invalid)?;
        draft.text = selection.text;
        if let Some(attachment) = selection.attach_thread {
            let environment = self.state.host_name.clone().unwrap_or_default();
            push_context_record(&mut draft, attachment.record(&environment));
        }
        if let Some(mode) = selection.interaction_mode {
            draft.interaction_mode = mode;
            if let Some(thread) = self.state.selected_thread.clone() {
                let entry = self.command(thread, Command::InteractionMode { mode });
                self.enqueue(entry, None)?;
            }
        }
        self.set_draft(draft);
        Ok(Next::Outcome(Outcome::ComposerEdited {
            cursor: selection.cursor,
        }))
    }

    /// Places the selected terminal lines at the caret, unless the draft
    /// already holds that range.
    pub(super) fn add_terminal_context(
        &mut self,
        text: String,
        cursor: u32,
        selection: &TerminalContextSelection,
    ) -> Next {
        let Some(selection) = normalize_terminal_context_selection(selection) else {
            return Next::Done;
        };
        let mut draft = self.state.current_draft();
        if context_records(&draft)
            .iter()
            .any(|record| is_same_terminal_range(&record.0, &selection))
        {
            return Next::Done;
        }
        let record = terminal_context_record(&uuid::Uuid::new_v4().to_string(), &selection);
        let inserted =
            insert_inline_context_references(&text, cursor, &[terminal_context_reference(&record)]);
        draft.text = inserted.text;
        push_context_record(&mut draft, record);
        self.set_draft(draft);
        Next::Outcome(Outcome::ComposerEdited {
            cursor: inserted.cursor,
        })
    }

    /// Places links to the threads at the caret, each thread once.
    pub(super) fn add_thread_contexts(
        &mut self,
        text: String,
        cursor: u32,
        thread_ids: &[String],
    ) -> Next {
        let mut draft = self.state.current_draft();
        let mut attached: Vec<String> = context_records(&draft)
            .iter()
            .filter_map(|record| context_id(record).map(str::to_owned))
            .collect();
        let environment = self.state.host_name.clone().unwrap_or_default();
        let mut references = vec![];
        for thread_id in thread_ids {
            let Some(title) = ThreadId::new(thread_id.clone())
                .ok()
                .and_then(|id| self.state.thread_row(&id).map(|row| row.title.clone()))
            else {
                continue;
            };
            let attachment = ThreadContextAttachment::new(thread_id, &title);
            if attached.contains(&attachment.context_id) {
                continue;
            }
            attached.push(attachment.context_id.clone());
            references.push(format_context_reference(
                "thread",
                &attachment.context_id,
                &attachment.label,
            ));
            push_context_record(&mut draft, attachment.record(&environment));
        }
        if references.is_empty() {
            return Next::Done;
        }
        let inserted = insert_inline_context_references(&text, cursor, &references);
        draft.text = inserted.text;
        self.set_draft(draft);
        Next::Outcome(Outcome::ComposerEdited {
            cursor: inserted.cursor,
        })
    }

    /// Adds visible terminal lines to the draft: their record, and a link at
    /// the end of the text.
    pub(super) fn attach_terminal_output(
        &mut self,
        output: &TerminalOutputContext,
    ) -> Result<Next, PeerError> {
        let mut draft = self.state.current_draft();
        let context = draft
            .context
            .get_or_insert_with(|| agent_domain::MessageContext {
                version: 1,
                records: vec![],
            });
        if context.records.len() >= agent_domain::COMPOSER_CONTEXT_MAX_RECORDS {
            return Err(invalid(TOO_MANY_CONTEXT_ITEMS));
        }
        let (record, reference) = output.record(&uuid::Uuid::new_v4().to_string());
        context.records.push(Json(record));
        draft.text = append_context_reference(&draft.text, &reference);
        self.set_draft(draft);
        Ok(Next::Done)
    }

    /// Removes a context record and the links to it from the draft.
    pub(super) fn remove_draft_context(&mut self, id: &str) {
        let mut draft = self.state.current_draft();
        if let Some(context) = draft.context.as_mut() {
            context
                .records
                .retain(|record| context_id(record) != Some(id));
            if context.records.is_empty() {
                draft.context = None;
            }
        }
        let link = regex::Regex::new(&format!(
            r"\[[^\]]*\]\(context://v1/[a-z0-9-]+/{}\)\s?",
            regex::escape(id)
        ))
        .expect("escaped pattern");
        draft.text = link.replace_all(&draft.text, "").into_owned();
        self.set_draft(draft);
    }

    pub(super) fn change_traits(&mut self, change: Option<TraitChange>) -> Result<Next, PeerError> {
        let Some(change) = change else {
            return Ok(Next::Done);
        };
        let mut draft = self.state.current_draft();
        if let Some(text) = change.text {
            draft.text = text;
        }
        let mut next = Next::Done;
        if let Some(options) = change.options {
            draft.options = options;
            remember_model_options(
                &mut self.state.preferences.model_options,
                &draft.instance_id,
                &draft.model,
                &draft.options,
            );
            let selection = draft.selection().map_err(invalid)?;
            self.state.default_draft.options = draft.options.clone();
            if let Some(thread) = self.state.selected_thread.clone() {
                next = Next::Commands(vec![self.command(thread, select_model_command(selection))]);
            }
        }
        self.set_draft(draft);
        Ok(next)
    }

    pub(super) fn select_trait(
        &mut self,
        descriptor: &str,
        choice: &str,
    ) -> Result<Next, PeerError> {
        let draft = self.state.current_draft();
        let change = select_trait(&catalog(&self.state), &draft, true, descriptor, choice);
        self.change_traits(change)
    }

    pub(super) fn toggle_trait(&mut self, descriptor: &str, on: bool) -> Result<Next, PeerError> {
        let draft = self.state.current_draft();
        let change = toggle_trait(&catalog(&self.state), &draft, descriptor, on);
        self.change_traits(change)
    }

    pub(super) fn stash_draft(&mut self) -> Result<Next, PeerError> {
        let mut draft = self.state.current_draft();
        if draft.is_empty() {
            return Err(invalid("Nothing to stash"));
        }
        let id = new_id("stash");
        let entry = new_stash_entry(id.clone(), now_ms() as i64, &draft, draft.context.clone())
            .map_err(invalid)?;
        if self.state.stash.stash(entry).is_some() {
            self.state.error = Some(evicted_entry_warning());
        }
        draft.text.clear();
        draft.attachments.clear();
        draft.context = None;
        self.set_draft(draft);
        Ok(Next::Outcome(Outcome::Stashed { entry_id: id }))
    }

    pub(super) fn finalize_stash_images(&mut self, id: &str, images: StashImages) {
        self.state.stash.finalize_images(id, images);
    }

    pub(super) fn restore_stash(&mut self, id: &str) -> Result<Next, PeerError> {
        let entry = self
            .state
            .stash
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
            .ok_or_else(|| invalid("That stashed prompt is gone"))?;
        if entry.pending_image_count > 0 {
            return Err(invalid("Wait for the stashed images to finish saving"));
        }
        self.state.stash.take(id);
        let mut draft = self.state.current_draft();
        let restore = restore_stash_entry(&entry, &draft, &[]);
        draft.text = restore.text;
        draft.attachments.extend(restore.files.iter().map(|file| {
            DraftAttachment::from_remote(&Attachment {
                kind: AttachmentKind::File,
                source: None,
                id: file.attachment_id.clone(),
                name: file.name.clone(),
                mime_type: file.mime_type.clone(),
                path: String::new(),
                size: file.size_bytes,
            })
        }));
        if let Some(restored) = entry.context {
            let context = draft
                .context
                .get_or_insert_with(|| agent_domain::MessageContext {
                    version: restored.version,
                    records: vec![],
                });
            for record in restored.records {
                if !context
                    .records
                    .iter()
                    .any(|existing| context_id(existing) == context_id(&record))
                {
                    context.records.push(record);
                }
            }
        }
        self.set_draft(draft);
        Ok(Next::Outcome(Outcome::StashRestored {
            images: restore.images,
            warning: restore.warning,
        }))
    }

    fn question_request(
        &self,
        thread: &ThreadId,
        request_id: &str,
    ) -> Result<(RuntimeRequestId, Vec<Question>), PeerError> {
        self.state
            .thread_state(thread)
            .and_then(|state| state.requests.iter().find(|r| r.id.as_str() == request_id))
            .and_then(|request| match &request.body {
                RequestBody::Questions { questions } => {
                    Some((request.id.clone(), questions.clone()))
                }
                RequestBody::Approval { .. } => None,
            })
            .ok_or_else(|| invalid("Question is unavailable"))
    }

    pub(super) fn edit_answer(
        &mut self,
        request_id: String,
        question_id: &str,
        edit: AnswerEdit,
    ) -> Result<Next, PeerError> {
        let thread = self.selected()?;
        let (_, questions) = self.question_request(&thread, &request_id)?;
        let question = questions
            .iter()
            .find(|question| question.id == question_id)
            .ok_or_else(|| invalid("Question is unavailable"))?;
        let drafts = self.state.question_drafts.entry(request_id).or_default();
        let current = drafts.get(question_id);
        let (next, displaced) = match edit {
            AnswerEdit::Custom { text } => {
                (set_question_custom_answer(question, current, &text), None)
            }
            AnswerEdit::ToggleOption { value } => (
                toggle_question_option(question, current, &value),
                current.map(|draft| draft.custom_answer.clone()),
            ),
        };
        drafts.set(next);
        if let Some(displaced) = displaced.filter(|text| !text.trim().is_empty()) {
            let mut draft = self.state.current_draft();
            draft.text = carry_displaced_custom_answer_into_prompt(&draft.text, &displaced);
            self.set_draft(draft);
        }
        Ok(Next::Done)
    }

    pub(super) fn submit_answers(&mut self, request_id: String) -> Result<Next, PeerError> {
        let thread = self.selected()?;
        let (request, questions) = self.question_request(&thread, &request_id)?;
        let drafts = answer_drafts(&self.state, &request_id);
        let answers = question_answers(&questions, &drafts)
            .ok_or_else(|| invalid("Answer every question"))?;
        let mut attachments = BTreeMap::new();
        for question in &questions {
            let key = answer_draft_key(&request_id, &question.id);
            if let Some(draft) = self.state.drafts.get(&key)
                && !draft.attachments.is_empty()
            {
                attachments.insert(
                    question.id.clone(),
                    draft.attachment_refs().map_err(invalid)?,
                );
            }
        }
        let command = crate::commands::build::answers_command(request, answers, attachments);
        let entry = self.command(thread, command);
        self.state.question_drafts.remove(&request_id);
        let prefix = answer_draft_key(&request_id, "");
        self.state.drafts.retain(|key, _| !key.starts_with(&prefix));
        Ok(Next::Commands(vec![entry]))
    }

    fn summaries(&self) -> Vec<ThreadSummary> {
        self.state
            .shell_view()
            .map(|shell| {
                shell
                    .threads
                    .iter()
                    .map(ThreadSummary::from_shell)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn move_thread(
        &mut self,
        moved: &str,
        section: OrderSection,
        destination: &MoveDestination,
    ) -> Result<Next, PeerError> {
        if self.state.thread_order.is_some() {
            return Ok(Next::Done);
        }
        let section = match destination {
            MoveDestination::Drop {
                section: Some(DropSection::Settled),
                ..
            } => {
                let thread = ThreadId::new(moved).map_err(invalid)?;
                return Ok(Next::Commands(vec![
                    self.lifecycle(thread, LifecycleAction::Settle),
                ]));
            }
            MoveDestination::Drop {
                section: Some(DropSection::Pinned),
                ..
            } => OrderSection::Pinned,
            MoveDestination::Drop {
                section: Some(DropSection::Active),
                ..
            } => OrderSection::Active,
            _ => section,
        };
        let threads = self.summaries();
        let Some(thread) = threads
            .iter()
            .find(|thread| thread.id == moved && thread.archived_at.is_none())
        else {
            return Ok(Next::Done);
        };
        let id = ThreadId::new(moved).map_err(invalid)?;
        // A drop names its section; the thread may come from another one.
        let section = match destination {
            MoveDestination::Drop {
                section: Some(DropSection::Settled),
                ..
            } => return Ok(Next::Commands(vec![self.lifecycle(id, LifecycleAction::Settle)])),
            MoveDestination::Drop {
                section: Some(DropSection::Pinned),
                ..
            } => OrderSection::Pinned,
            MoveDestination::Drop {
                section: Some(DropSection::Active),
                ..
            } => OrderSection::Active,
            _ => section,
        };
        let now = now_ms() as i64;
        let queued = queued_threads(&self.state);
        let ordered = ordered_section(&threads, section, None, now, &queued);
        let Some(assignments) =
            ThreadMovePlanner::new(&ordered, Some(&threads), section).plan(moved, destination)
        else {
            return Ok(Next::Done);
        };
        if !ordered.iter().any(|row| row.id == moved) {
            return self.move_across_sections(moved, section, &threads, assignments, now);
        }
        let Some(order) =
            PendingThreadOrder::begin(section, &ordered, moved, destination, &assignments)
        else {
            return Ok(Next::Done);
        };
        let entries = assignments
            .into_iter()
            .map(|assignment| {
                let thread = ThreadId::new(assignment.id.clone()).map_err(invalid)?;
                Ok(self.lifecycle(thread, match section {
                    OrderSection::Pinned => LifecycleAction::ReorderPinned { order: assignment.order_key },
                    OrderSection::Active => LifecycleAction::ReorderActive { order: assignment.order_key },
                }))
            })
            .collect::<Result<Vec<_>, PeerError>>()?;
        self.state.thread_order = Some(ThreadOrderHold {
            order,
            commands: entries.iter().map(|entry| entry.id.clone()).collect(),
        });
        Ok(Next::Commands(entries))
    }

    /// A drop from another section: pinning carries the moved thread's key; a
    /// drop on Active first clears the pin, settlement and snooze it leaves.
    fn move_across_sections(
        &mut self,
        moved: &str,
        section: OrderSection,
        threads: &[ThreadSummary],
        assignments: Vec<crate::view::thread_sort::OrderAssignment>,
        now: i64,
    ) -> Result<Next, PeerError> {
        let Some(summary) = threads.iter().find(|thread| thread.id == moved) else {
            return Ok(Next::Done);
        };
        let thread = ThreadId::new(moved).map_err(invalid)?;
        let lifecycle = thread_drop_lifecycle(summary, section, now);
        let mut actions = vec![];
        if lifecycle.pin {
            let order = assignments
                .iter()
                .find(|assignment| assignment.id == moved)
                .map(|assignment| assignment.order_key.clone());
            actions.push((thread.clone(), LifecycleAction::Pin { order }));
        }
        for (apply, action) in [
            (lifecycle.unpin, LifecycleAction::Unpin),
            (lifecycle.unsettle, LifecycleAction::Unsettle),
            (lifecycle.unsnooze, LifecycleAction::Unsnooze),
        ] {
            if apply {
                actions.push((thread.clone(), action));
            }
        }
        for assignment in assignments {
            if lifecycle.pin && summary.pinned_at.is_none() && assignment.id == moved {
                continue;
            }
            let id = ThreadId::new(assignment.id).map_err(invalid)?;
            actions.push((
                id,
                match section {
                    OrderSection::Pinned => LifecycleAction::ReorderPinned {
                        order: assignment.order_key,
                    },
                    OrderSection::Active => LifecycleAction::ReorderActive {
                        order: assignment.order_key,
                    },
                },
            ));
        }
        Ok(Next::Commands(
            actions
                .into_iter()
                .map(|(thread, action)| self.lifecycle(thread, action))
                .collect(),
        ))
    }

    /// Reconciles the held list order with the shell and the outbox; the hold
    /// ends once its writes landed, or at once when one failed.
    pub(super) fn refresh_thread_order(&mut self) {
        let Some(hold) = self.state.thread_order.clone() else {
            return;
        };
        let threads = self.summaries();
        let queued = queued_threads(&self.state);
        let now = now_ms() as i64;
        let complete = hold
            .commands
            .iter()
            .all(|id| self.state.outbox.get(id).is_none());
        let order = if complete && !hold.order.commands_complete {
            hold.order.complete(&threads, now, &queued)
        } else {
            hold.order.refresh(&threads, now, &queued)
        };
        self.state.thread_order = order.map(|order| ThreadOrderHold {
            order,
            commands: hold.commands,
        });
    }

    pub(super) fn drop_thread(
        &mut self,
        thread: ThreadId,
        plan: SidebarThreadDropPlan,
    ) -> Result<Next, PeerError> {
        let mut actions: Vec<(ThreadId, LifecycleAction)> = vec![];
        let assignment = |assignment: crate::view::thread_sort::OrderAssignment,
                          pinned: bool|
         -> Result<(ThreadId, LifecycleAction), PeerError> {
            let id = ThreadId::new(assignment.id).map_err(invalid)?;
            Ok((
                id,
                if pinned {
                    LifecycleAction::ReorderPinned {
                        order: assignment.order_key,
                    }
                } else {
                    LifecycleAction::ReorderActive {
                        order: assignment.order_key,
                    }
                },
            ))
        };
        match plan {
            SidebarThreadDropPlan::None => return Ok(Next::Done),
            SidebarThreadDropPlan::ReorderPinned { assignments, .. } => {
                for item in assignments {
                    actions.push(assignment(item, true)?);
                }
            }
            SidebarThreadDropPlan::Pin {
                order_key,
                extra_assignments,
                ..
            } => {
                actions.push((thread, LifecycleAction::Pin { order: order_key }));
                for item in extra_assignments {
                    actions.push(assignment(item, true)?);
                }
            }
            SidebarThreadDropPlan::MoveActive {
                assignments,
                unpin,
                unsettle,
                unsnooze,
                ..
            } => {
                for (apply, action) in [
                    (unpin, LifecycleAction::Unpin),
                    (unsettle, LifecycleAction::Unsettle),
                    (unsnooze, LifecycleAction::Unsnooze),
                ] {
                    if apply {
                        actions.push((thread.clone(), action));
                    }
                }
                for item in assignments {
                    actions.push(assignment(item, false)?);
                }
            }
            SidebarThreadDropPlan::Settle => actions.push((thread, LifecycleAction::Settle)),
        }
        Ok(Next::Commands(
            actions
                .into_iter()
                .map(|(thread, action)| self.lifecycle(thread, action))
                .collect(),
        ))
    }

    pub(super) fn limit_recovery(
        &mut self,
        thread: ThreadId,
        action: RecoveryAction,
    ) -> Result<Next, PeerError> {
        let now = now_ms() as i64;
        let recovery = self
            .state
            .thread_row(&thread)
            .and_then(|row| usage_limit_recovery(row, now))
            .ok_or_else(|| invalid("The usage limit has passed"))?;
        match toggle_limit_recovery(&recovery, action, now) {
            RecoveryToggle::Ignored => Ok(Next::Done),
            RecoveryToggle::Rejected(message) => Err(invalid(message)),
            RecoveryToggle::Send(update) => Ok(Next::Commands(vec![self.command(
                thread,
                metadata_command(MetadataChange {
                    limit_recovery: Some(Some(update)),
                    ..MetadataChange::default()
                }),
            )])),
        }
    }

    fn diff_selection(
        &mut self,
        thread: &ThreadId,
    ) -> &mut crate::view::checkpoints::DiffPanelSelection {
        self.state.diff_panels.entry(thread.clone()).or_default()
    }

    pub(super) fn select_diff_scope(
        &mut self,
        choice: &DiffScopeChoice,
    ) -> Result<Next, PeerError> {
        let thread = self.selected()?;
        let turns = self
            .state
            .thread_state(&thread)
            .map(checkpoint_summaries)
            .unwrap_or_default();
        self.diff_selection(&thread).select_scope(choice, &turns);
        self.load_diff()
    }

    pub(super) fn select_diff_turn(
        &mut self,
        run_id: &str,
        file_path: Option<&str>,
    ) -> Result<Next, PeerError> {
        let thread = self.selected()?;
        self.diff_selection(&thread).select_turn(run_id, file_path);
        self.load_diff()
    }

    pub(super) fn select_diff_base_ref(
        &mut self,
        base_ref: Option<&str>,
    ) -> Result<Next, PeerError> {
        let thread = self.selected()?;
        self.diff_selection(&thread)
            .select_branch_base_ref(base_ref);
        self.load_diff()
    }

    /// Requests what the open thread's diff panel shows.
    pub(super) fn load_diff(&mut self) -> Result<Next, PeerError> {
        let thread = self.selected()?;
        let turns = self
            .state
            .thread_state(&thread)
            .map(checkpoint_summaries)
            .unwrap_or_default();
        let selection = self.diff_selection(&thread).clone();
        let panel = diff_panel(
            &turns,
            &selection,
            self.state.preferences.diff_ignore_whitespace,
        );
        let cwd = self.state.cwd();
        let (base_ref, ignore_whitespace) = match panel.request {
            Some(crate::view::checkpoints::DiffRequest::Branch {
                base_ref,
                ignore_whitespace,
            }) => (base_ref, ignore_whitespace),
            Some(crate::view::checkpoints::DiffRequest::Unstaged { ignore_whitespace }) => {
                (None, ignore_whitespace)
            }
            Some(request) => {
                return match request.intent() {
                    Some(intent) => self.prepare(intent),
                    None => Ok(Next::Done),
                };
            }
            None => return Ok(Next::Done),
        };
        if cwd.is_empty() {
            return Ok(Next::Done);
        }
        self.load_vcs_status(cwd.clone());
        let call = self.load_diff_preview(cwd, base_ref, ignore_whitespace);
        self.show_diff_preview();
        Ok(Next::call(call, None))
    }

    pub(super) fn toggle_favorite_model(&mut self, instance_id: &str, model: &str) {
        let preferences = &mut self.state.preferences;
        preferences.favorite_models =
            toggle_favorite(&preferences.favorite_models, instance_id, model);
    }
}

impl Owner {
    /// Keeps the list's device holds current: when threads left the Working
    /// section, and the reorder held on screen.
    pub(super) fn observe_list(&mut self) {
        let working = self.state.preferences.working_section;
        if let Some((shell, outbox, observed_working)) = &self.observed_list
            && Arc::ptr_eq(shell, &self.state.shell)
            && Arc::ptr_eq(outbox, &self.state.outbox)
            && *observed_working == working
        {
            return;
        }
        let threads = self.summaries();
        self.state
            .inbox_returns
            .observe(working.then_some(threads.as_slice()), now_ms() as i64);
        self.refresh_thread_order();
        self.observed_list = Some((self.state.shell.clone(), self.state.outbox.clone(), working));
    }
}
