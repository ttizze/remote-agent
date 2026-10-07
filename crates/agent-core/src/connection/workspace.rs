//! Workspace reads behind the composer menu, the diff panel and the new-task
//! branch picker: provider commands, `@` path search, Git status, refs and
//! diff previews.
use super::{
    intents::{Next, invalid},
    owner::{Owner, new_id, now_ms},
};
use crate::{
    commands::{
        build::{LaunchThread, TurnMessage, WorkspaceChoice, launch, thread_title_seed},
        outbox::{PendingCommand, Request, Restore},
    },
    peer::PeerError,
    protocol::Call,
    state::{
        DiffPreviewEntry, DraftWorkspace, EntryQuery, PROVIDER_COMMANDS_RETRY_MS, RefScope,
        RefsEntry,
    },
    view::{
        checkpoints::DiffSelection,
        composer::commands::{ComposerTriggerKind, detect_composer_trigger},
        projects::selection::ThreadWorkspaceMode,
        timeline::rows::TimelineLayout,
    },
};
use agent_domain::{MessageId, Role, ThreadId};
use agent_protocol::workspace as w;
use std::sync::Arc;

/// How long the composer waits after typing before searching paths, and how
/// many it asks for.
pub(crate) fn path_search_rules(layout: TimelineLayout) -> (u64, u32) {
    match layout {
        TimelineLayout::Mobile => (200, 20),
        TimelineLayout::Desktop => (120, 80),
    }
}

/// Branches a ref listing asks for at once.
pub(crate) const REF_LIST_LIMIT: u32 = 100;

impl Owner {
    /// The composer's text or cursor changed: loads the provider's commands
    /// for its directory and schedules the `@` path search.
    pub(super) fn update_composer_menu(&mut self, text: &str, cursor: u32, layout: TimelineLayout) {
        self.state.sources.composer_layout = layout;
        let draft = self.state.current_draft();
        let cwd = self.state.composer_cwd();
        if !draft.instance_id.is_empty() && !cwd.is_empty() {
            self.ensure_provider_commands(&draft.instance_id, &cwd);
        }
        let trigger = detect_composer_trigger(text, cursor);
        let query = trigger
            .filter(|trigger| trigger.kind == ComposerTriggerKind::Path)
            .map(|trigger| trigger.query.trim().to_owned())
            .filter(|query| !query.is_empty() && !cwd.is_empty());
        let entries = &mut self.state.sources.entries;
        let Some(query) = query else {
            entries.wanted = None;
            entries.due_at_ms = None;
            return;
        };
        let (debounce, limit) = path_search_rules(layout);
        let wanted = EntryQuery { cwd, query, limit };
        if entries.wanted.as_ref() == Some(&wanted) {
            return;
        }
        entries.wanted = Some(wanted);
        entries.due_at_ms = Some(now_ms() + debounce);
    }

    pub(super) fn ensure_provider_commands(&mut self, instance: &str, cwd: &str) {
        if !self.connected() {
            return;
        }
        let now = now_ms();
        let entry = self
            .state
            .sources
            .provider_commands
            .entry((instance.to_owned(), cwd.to_owned()))
            .or_default();
        if entry.complete() || entry.in_flight || entry.retry_at_ms.is_some_and(|retry| retry > now)
        {
            return;
        }
        let fresh = entry.retry_at_ms.is_some();
        entry.in_flight = true;
        entry.retry_at_ms = None;
        self.job(
            Call::ProviderCommands(w::ListProviderCommands {
                instance: instance.into(),
                cwd: cwd.into(),
                fresh,
            }),
            None,
            None,
        );
    }

    pub(super) fn provider_commands_finished(
        &mut self,
        request: &w::ListProviderCommands,
        result: Result<w::ProviderCommands, &PeerError>,
    ) {
        let entry = self
            .state
            .sources
            .provider_commands
            .entry((request.instance.clone(), request.cwd.clone()))
            .or_default();
        entry.in_flight = false;
        match result {
            Ok(commands) => {
                entry.retry_at_ms = commands
                    .slash_commands_pending
                    .then(|| now_ms() + PROVIDER_COMMANDS_RETRY_MS);
                entry.commands = Some(commands);
            }
            Err(_) => entry.retry_at_ms = Some(now_ms() + PROVIDER_COMMANDS_RETRY_MS),
        }
    }

    /// The next time a debounced search or a provider command retry is due.
    pub(super) fn sources_deadline(&self) -> Option<u64> {
        let sources = &self.state.sources;
        let retry = sources
            .provider_commands
            .values()
            .filter(|entry| !entry.in_flight)
            .filter_map(|entry| entry.retry_at_ms);
        sources.entries.due_at_ms.into_iter().chain(retry).min()
    }

    /// Asks the debounced path search and retries the composer's provider
    /// commands once their cooldown passes.
    pub(super) fn sources_tick(&mut self, now: u64) {
        let entries = &mut self.state.sources.entries;
        if entries.due_at_ms.is_some_and(|due| due <= now) {
            entries.due_at_ms = None;
            if let Some(wanted) = entries.wanted.clone() {
                self.job(
                    Call::SearchEntries(w::SearchEntries {
                        cwd: wanted.cwd,
                        query: wanted.query,
                        limit: wanted.limit,
                        kind: None,
                        image_only: false,
                    }),
                    None,
                    None,
                );
            }
        }
        let due: Vec<(String, String)> = self
            .state
            .sources
            .provider_commands
            .iter()
            .filter(|(_, entry)| !entry.in_flight && entry.retry_at_ms.is_some_and(|at| at <= now))
            .map(|(key, _)| key.clone())
            .collect();
        let draft = self.state.current_draft();
        let cwd = self.state.composer_cwd();
        for (instance, directory) in due {
            if instance == draft.instance_id && directory == cwd {
                self.ensure_provider_commands(&instance, &directory);
            } else if let Some(entry) = self
                .state
                .sources
                .provider_commands
                .get_mut(&(instance, directory))
            {
                entry.retry_at_ms = None;
            }
        }
    }

    pub(super) fn entries_found(&mut self, request: &w::SearchEntries, found: w::EntrySearch) {
        let query = EntryQuery {
            cwd: request.cwd.clone(),
            query: request.query.clone(),
            limit: request.limit,
        };
        let entries = &mut self.state.sources.entries;
        if entries.wanted.as_ref() == Some(&query) {
            entries.result = Some((query, found.entries));
        }
    }

    /// Lists a checkout's branches for a picker.
    pub(super) fn load_refs(&mut self, cwd: String, scope: RefScope, query: String) {
        if cwd.is_empty() || !self.connected() {
            return;
        }
        let query = query.trim().to_owned();
        let entry = self
            .state
            .sources
            .refs
            .entry((cwd.clone(), scope))
            .or_default();
        if entry.query == query && (entry.in_flight || entry.list.is_some()) {
            return;
        }
        let keep = entry.query == query;
        *entry = RefsEntry {
            query: query.clone(),
            list: if keep { entry.list.take() } else { None },
            error: None,
            in_flight: true,
        };
        self.job(
            Call::ListRefs(w::ListRefs {
                cwd,
                query: (!query.is_empty()).then_some(query),
                cursor: None,
                include_matching_remote_refs: scope != RefScope::All,
                ref_kind: scope.kind(),
                limit: Some(REF_LIST_LIMIT),
            }),
            None,
            None,
        );
    }

    pub(super) fn refs_finished(
        &mut self,
        request: &w::ListRefs,
        result: Result<w::RefList, &PeerError>,
    ) {
        let scope = match request.ref_kind {
            w::RefKind::Local => RefScope::Local,
            w::RefKind::Remote => RefScope::Remote,
            w::RefKind::All => RefScope::All,
        };
        let Some(entry) = self
            .state
            .sources
            .refs
            .get_mut(&(request.cwd.clone(), scope))
        else {
            return;
        };
        if entry.query != request.query.clone().unwrap_or_default() {
            return;
        }
        entry.in_flight = false;
        match result {
            Ok(list) => entry.list = Some(list),
            Err(error) => {
                entry.error = Some(crate::presentation::error::error_message(
                    &error.to_string(),
                ))
            }
        }
    }

    pub(super) fn load_vcs_status(&mut self, cwd: String) {
        if cwd.is_empty() || !self.connected() {
            return;
        }
        self.job(Call::VcsStatus(w::ReadVcsStatus { cwd }), None, None);
    }

    /// Loads both diffs of a checkout; the panel shows the one its scope picks.
    pub(super) fn load_diff_preview(
        &mut self,
        cwd: String,
        base_ref: Option<String>,
        ignore_whitespace: bool,
    ) -> Call {
        let request = w::DiffPreview {
            cwd,
            base_ref,
            ignore_whitespace,
            file: None,
        };
        let previous = self.state.sources.diff_preview.take();
        self.state.sources.diff_preview = Some(DiffPreviewEntry {
            result: previous
                .filter(|entry| entry.request == request)
                .and_then(|entry| entry.result),
            request: request.clone(),
            error: None,
        });
        self.state.workspace.diff_request = None;
        Call::DiffPreview(request)
    }

    pub(super) fn diff_preview_finished(
        &mut self,
        request: &w::DiffPreview,
        result: Result<w::DiffPreviewResult, &PeerError>,
    ) {
        let Some(entry) = self
            .state
            .sources
            .diff_preview
            .as_mut()
            .filter(|entry| &entry.request == request)
        else {
            return;
        };
        match result {
            Ok(preview) => {
                entry.result = Some(Arc::new(preview));
                entry.error = None;
            }
            Err(error) => {
                entry.error = Some(crate::presentation::error::error_message(
                    &error.to_string(),
                ))
            }
        }
    }
}

impl Owner {
    /// Shows the open thread's Changes or Uncommitted diff from the preview.
    pub(super) fn show_diff_preview(&mut self) {
        let Some(thread) = self.state.selected_thread.clone() else {
            return;
        };
        let kind = match self
            .state
            .diff_panels
            .get(&thread)
            .map(|panel| &panel.selection)
        {
            Some(DiffSelection::Unstaged) => w::DiffSourceKind::WorkingTree,
            Some(DiffSelection::Turn { .. }) => return,
            _ => w::DiffSourceKind::BranchRange,
        };
        let Some(source) = self
            .state
            .sources
            .diff_preview
            .as_ref()
            .filter(|entry| entry.request.cwd == self.state.cwd())
            .and_then(|entry| entry.result.as_ref())
            .and_then(|preview| preview.sources.iter().find(|source| source.kind == kind))
        else {
            return;
        };
        let mut review = crate::presentation::diff::review_from_patch(source.diff.clone());
        review.branch = source.title.clone();
        let workspace = &mut self.state.workspace;
        workspace.review_generation += 1;
        workspace.review = Some(Arc::new(review));
    }

    /// "Work locally": stops the open thread's worktree setup, then starts its
    /// first message again as a new thread on the project's checkout.
    pub(super) fn work_locally(&mut self) -> Result<Next, PeerError> {
        let thread = self.selected()?;
        self.work_locally = Some(thread.clone());
        Ok(Next::call(
            Call::CancelSetup(agent_protocol::conversation::CancelSetup { thread_id: thread }),
            None,
        ))
    }

    pub(super) fn setup_cancelled(&mut self, thread: &ThreadId, cancelled: bool) {
        if self.work_locally.as_ref() != Some(thread) {
            return;
        }
        self.work_locally = None;
        if !cancelled {
            return;
        }
        let Some(state) = self.state.thread_state(thread) else {
            return;
        };
        let (Some(current), Some(message)) = (
            state.thread.as_ref(),
            state
                .messages
                .iter()
                .find(|message| message.role == Role::User),
        ) else {
            return;
        };
        let Ok(id) = MessageId::new(new_id("message")) else {
            return;
        };
        let Ok(child) = ThreadId::new(new_id("thread")) else {
            return;
        };
        let names: Vec<&str> = message
            .attachments
            .iter()
            .map(|attachment| attachment.name.as_str())
            .collect();
        let title_seed = thread_title_seed(&message.text, &names, &[]);
        let launch = launch(LaunchThread {
            command_id: self.new_command_id(),
            thread: Some(child.clone()),
            project: current.project.clone(),
            title: title_seed.clone(),
            title_seed: Some(title_seed),
            selection: current.selection.clone(),
            runtime_mode: current.runtime_mode,
            interaction_mode: current.interaction_mode,
            workspace: WorkspaceChoice::Local {
                branch: None,
                worktree_path: None,
            },
            message: Some(TurnMessage {
                id,
                text: message.text.clone(),
                attachments: message.attachments.clone(),
                context: message.context.clone(),
            }),
            creation_source: self.options.creation_source.clone(),
        });
        let mut entry = PendingCommand::new(child, Request::Launch(Box::new(launch)), self.now());
        entry.navigate = true;
        entry.restore = Some(Restore {
            draft_key: thread.to_string(),
            text: message.text.clone(),
            attachments: message.attachments.clone(),
            context: message.context.clone(),
        });
        if let Err(error) = self.enqueue(entry, None) {
            self.state.error = Some(error.to_string());
        }
    }

    /// The new-thread draft's workspace, filled in from the project's checkout
    /// and the Host's default.
    fn new_thread_workspace(&self) -> DraftWorkspace {
        self.state.new_thread_workspace()
    }

    pub(super) fn set_new_thread_workspace(
        &mut self,
        mode: ThreadWorkspaceMode,
    ) -> Result<Next, PeerError> {
        if self.state.new_thread_project_root().is_none() {
            return Err(invalid("Choose a project first"));
        }
        let current = self.new_thread_workspace();
        let next = match mode {
            ThreadWorkspaceMode::Local => {
                let local = self.state.new_thread_local_selection();
                DraftWorkspace {
                    mode,
                    branch: local.0,
                    worktree_path: local.1,
                    ..current
                }
            }
            ThreadWorkspaceMode::Worktree => DraftWorkspace { mode, ..current },
        };
        self.update_new_thread_draft(|draft| draft.workspace = Some(next));
        Ok(Next::Done)
    }

    pub(super) fn select_new_thread_branch(
        &mut self,
        branch: String,
        worktree_path: Option<String>,
    ) -> Result<Next, PeerError> {
        let root = self
            .state
            .new_thread_project_root()
            .ok_or_else(|| invalid("Choose a project first"))?;
        let current = self.new_thread_workspace();
        let worktree_path = crate::view::new_thread::branch_worktree_path(
            current.mode,
            &root,
            worktree_path.as_deref(),
        );
        self.update_new_thread_draft(|draft| {
            draft.workspace = Some(DraftWorkspace {
                branch: Some(branch),
                worktree_path,
                ..current
            })
        });
        Ok(Next::Done)
    }

    pub(super) fn set_new_thread_start_from_origin(&mut self, on: bool) -> Result<Next, PeerError> {
        let current = self.new_thread_workspace();
        self.update_new_thread_draft(|draft| {
            draft.workspace = Some(DraftWorkspace {
                start_from_origin: on,
                ..current
            })
        });
        Ok(Next::Done)
    }

    fn update_new_thread_draft(&mut self, change: impl FnOnce(&mut crate::state::Draft)) {
        let key = self.state.new_thread_draft_key();
        let mut draft = self
            .state
            .drafts
            .get(&key)
            .cloned()
            .unwrap_or_else(|| self.state.default_draft.clone());
        change(&mut draft);
        self.state.drafts.insert(key, draft);
    }

    /// Loads the refs and status the new-thread workspace controls show.
    pub(super) fn load_new_thread_branches(&mut self, query: String) {
        if let Some(root) = self.state.new_thread_project_root() {
            self.load_vcs_status(root.clone());
            self.load_refs(root, RefScope::All, query);
        }
    }
}
