//! Shell, thread and setup subscriptions: opening them from the folded cursor,
//! applying their items, retrying refusals and keeping recent threads.
use super::{
    owner::{Event, Owner, Stream, StreamKey, new_id, now_ms, timestamp},
    streams::{Payload, Target, follow, rpc_failure},
};
use crate::{
    commands::build::{LifecycleAction, lifecycle_command},
    peer::PeerError,
    protocol::Call,
    sync::{
        ShellCache, ThreadCacheEntry, ThreadStatus, ThreadSync, resubscribe_delay_ms,
        thread::{Applied, FailureAction, LoadEarlier, NOT_CONNECTED, Resync, RollbackResult},
    },
};
use agent_domain::WorktreeSetupSnapshot;
use agent_domain::{Item, RunStatus, ThreadId, TurnItemId};
use agent_protocol::conversation::{
    GetTurnItem, HistoryPage, ReadHistory, ShellLocation, ShellUpdate, SubscribeSetup, ThreadUpdate,
};
use std::{sync::Arc, time::Duration};

impl Owner {
    fn next_generation(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    fn open_stream(&mut self, key: StreamKey, call: Call) {
        let generation = self.next_generation();
        let sender = self.sender.clone();
        let Some(network) = self.network.as_mut().filter(|_| self.state.connected) else {
            return;
        };
        let target = Target {
            peer: network.peer.clone(),
            epoch: network.epoch,
            key: key.clone(),
            generation,
            sender,
        };
        let task = match &key {
            StreamKey::Shell | StreamKey::Archive => {
                tokio::spawn(follow(target, call, Payload::Shell))
            }
            StreamKey::Thread(_) => tokio::spawn(follow(target, call, Payload::Thread)),
            StreamKey::Setup(_) => tokio::spawn(follow(target, call, Payload::Setup)),
            StreamKey::TerminalMetadata => {
                tokio::spawn(follow(target, call, Payload::TerminalMetadata))
            }
            StreamKey::Keybindings => tokio::spawn(follow(target, call, Payload::Keybindings)),
            StreamKey::VcsStatus(_) => tokio::spawn(follow(target, call, Payload::VcsStatus)),
            StreamKey::GitAction(_) => {
                tokio::spawn(follow(target, call, Payload::ActionProgress))
            }
        };
        let failures = network
            .streams
            .get(&key)
            .map_or(0, |stream| stream.failures);
        network.streams.insert(
            key,
            Stream {
                generation,
                failures,
                task: Some(tokio_util::task::AbortOnDropHandle::new(task)),
            },
        );
    }

    pub(super) fn close_stream(&mut self, key: &StreamKey) {
        if let Some(network) = self.network.as_mut() {
            network.streams.remove(key);
        }
    }

    fn shell_mut(&mut self, location: ShellLocation) -> Option<&mut ShellCache> {
        match location {
            ShellLocation::Active => Some(Arc::make_mut(&mut self.state.shell)),
            ShellLocation::Archived => self.state.archived.as_mut().map(Arc::make_mut),
        }
    }

    pub(super) fn subscribe_shell(&mut self, location: ShellLocation) {
        if !self.connected() {
            return;
        }
        let epoch = self.epoch;
        let Some(shell) = self.shell_mut(location) else {
            return;
        };
        let request = shell.subscribe(epoch);
        let key = match location {
            ShellLocation::Active => StreamKey::Shell,
            ShellLocation::Archived => StreamKey::Archive,
        };
        self.open_stream(key, Call::SubscribeShell(request));
    }

    pub(super) fn subscribe_thread(&mut self, thread: &ThreadId) {
        if !self.connected() {
            return;
        }
        let Some(sync) = self.state.threads.get_mut(thread) else {
            return;
        };
        match Arc::make_mut(sync).subscribe(thread) {
            Some(request) => {
                self.open_stream(
                    StreamKey::Thread(thread.clone()),
                    Call::SubscribeThread(request),
                );
                self.subscribe_setup(thread);
            }
            None => self.close_thread_streams(thread),
        }
    }

    fn subscribe_setup(&mut self, thread: &ThreadId) {
        self.open_stream(
            StreamKey::Setup(thread.clone()),
            Call::SetupStream(SubscribeSetup {
                thread_id: thread.clone(),
            }),
        );
    }

    /// Every thread's terminal labels and running processes.
    pub(super) fn subscribe_terminal_metadata(&mut self) {
        if !self.connected() {
            return;
        }
        self.open_stream(
            StreamKey::TerminalMetadata,
            Call::TerminalMetadata(agent_protocol::models::Empty {}),
        );
    }

    /// The Host's keybindings and every change to them.
    pub(super) fn subscribe_keybindings(&mut self) {
        if !self.connected() {
            return;
        }
        self.open_stream(
            StreamKey::Keybindings,
            Call::Keybindings(agent_protocol::models::Empty {}),
        );
    }

    /// Subscribes to one checkout's local and remote Git status.
    pub(super) fn subscribe_vcs_status(&mut self, cwd: String) {
        if !self.connected() || cwd.is_empty() {
            return;
        }
        self.open_stream(
            StreamKey::VcsStatus(cwd.clone()),
            Call::SubscribeVcsStatus(agent_protocol::vcs::SubscribeVcsStatus { cwd }),
        );
    }

    /// Starts one checkout stream without replacing a healthy subscription.
    pub(super) fn ensure_vcs_status(&mut self, cwd: String) {
        if !self.connected() || cwd.is_empty() {
            return;
        }
        let key = StreamKey::VcsStatus(cwd.clone());
        if self
            .network
            .as_ref()
            .is_some_and(|network| network.streams.contains_key(&key))
        {
            return;
        }
        self.subscribe_vcs_status(cwd);
    }

    pub(super) fn ensure_selected_vcs_status(&mut self) {
        let Some(thread) = self.state.selected_thread.clone() else {
            return;
        };
        self.ensure_vcs_status(self.state.thread_cwd(&thread));
    }

    pub(super) fn start_git_action(&mut self, request: agent_protocol::vcs::RunStackedAction) {
        if !self.connected() {
            return;
        }
        let key = StreamKey::GitAction(request.action_id.clone());
        self.open_stream(key, Call::RunStackedAction(request));
    }

    fn terminal_metadata(&mut self, event: agent_protocol::operations::TerminalMetadataEvent) {
        use agent_protocol::operations::TerminalMetadataEvent as Metadata;
        let terminals = &mut self.state.terminal_metadata;
        match event {
            Metadata::Snapshot { terminals: all } => {
                *terminals = all
                    .into_iter()
                    .map(|summary| {
                        (
                            (summary.thread.clone(), summary.terminal_id.clone()),
                            summary,
                        )
                    })
                    .collect();
            }
            Metadata::Upsert { terminal } => {
                terminals.insert(
                    (terminal.thread.clone(), terminal.terminal_id.clone()),
                    terminal,
                );
            }
            Metadata::Remove {
                thread,
                terminal_id,
            } => {
                terminals.remove(&(thread, terminal_id));
            }
        }
    }

    fn close_thread_streams(&mut self, thread: &ThreadId) {
        self.close_stream(&StreamKey::Thread(thread.clone()));
        self.close_stream(&StreamKey::Setup(thread.clone()));
    }

    /// The archive is subscribed only while it is shown.
    pub(super) fn show_archived(&mut self, open: bool) {
        if open {
            if self.state.archived.is_none() {
                self.state.archived = Some(Arc::new(ShellCache::new(ShellLocation::Archived)));
            }
            self.subscribe_shell(ShellLocation::Archived);
        } else {
            self.state.archived = None;
            self.close_stream(&StreamKey::Archive);
        }
    }

    /// A thread gains its live view: from memory, else from the disk cache.
    pub(super) fn open_thread(&mut self, thread: &ThreadId) {
        let sync = match self.state.threads.get(thread) {
            Some(retained) => retained.resumed(),
            None => {
                let cached = self
                    .cache
                    .as_ref()
                    .and_then(|cache| cache.load_thread(thread));
                let entry = cached
                    .as_ref()
                    .map_or_else(ThreadCacheEntry::default, ThreadCacheEntry::loaded);
                self.thread_caches.insert(thread.clone(), entry);
                cached.map_or_else(ThreadSync::default, ThreadSync::from_cache)
            }
        };
        let mut sync = sync;
        sync.open();
        self.state.threads.insert(thread.clone(), Arc::new(sync));
        self.thread_caches.entry(thread.clone()).or_default();
        self.last_used.insert(thread.clone(), now_ms());
        self.subscribe_thread(thread);
    }

    /// The previous thread keeps its folded state for the retention period.
    pub(super) fn select_thread(&mut self, thread: Option<ThreadId>) {
        if self.state.selected_thread == thread {
            if let Some(thread) = &thread
                && !self.state.threads.contains_key(thread)
            {
                self.open_thread(thread);
                self.ensure_selected_vcs_status();
            }
            return;
        }
        if let Some(previous) = self.state.selected_thread.take() {
            self.close_thread_streams(&previous);
            self.last_used.insert(previous.clone(), now_ms());
            self.store_thread_now(&previous);
        }
        self.state.selected_thread = thread.clone();
        self.state.editing_run = None;
        let workspace = &mut self.state.workspace;
        workspace.review = None;
        workspace.diff_request = None;
        workspace.directory = None;
        workspace.listed_directory = None;
        workspace.file = None;
        workspace.requested_directory = None;
        workspace.requested_file = None;
        if let Some(thread) = thread {
            self.visited.remove(&thread);
            self.open_thread(&thread);
            self.ensure_selected_vcs_status();
            self.visit_selected();
        }
    }

    pub(super) fn app_became_active(&mut self) {
        if !self.connected() {
            return;
        }
        self.subscribe_shell(ShellLocation::Active);
        if self.state.archived.is_some() {
            self.subscribe_shell(ShellLocation::Archived);
        }
        if let Some(thread) = self.state.selected_thread.clone() {
            self.subscribe_thread(&thread);
        }
        self.subscribe_git_statuses();
    }

    pub(super) fn subscribe_git_statuses(&mut self) {
        let checkouts: Vec<String> = self.state.git.status_events.keys().cloned().collect();
        for cwd in checkouts {
            self.subscribe_vcs_status(cwd);
        }
        // A restored device has no persisted Git status events. The selected
        // checkout still needs its first snapshot so the desktop toolbar and
        // native controls can render immediately after reconnecting.
        self.ensure_selected_vcs_status();
    }

    fn current(&self, key: &StreamKey, generation: u64) -> bool {
        self.network
            .as_ref()
            .and_then(|network| network.streams.get(key))
            .is_some_and(|stream| stream.generation == generation)
    }

    fn healthy(&mut self, key: &StreamKey) {
        if let Some(stream) = self.network.as_mut().and_then(|n| n.streams.get_mut(key)) {
            stream.failures = 0;
        }
    }

    /// Retries a refused or dropped subscription on the same connection.
    fn schedule_resubscribe(&mut self, key: StreamKey) {
        let generation = self.next_generation();
        let sender = self.sender.clone();
        let Some(network) = self.network.as_mut() else {
            return;
        };
        let epoch = network.epoch;
        let stream = network.streams.entry(key.clone()).or_insert(Stream {
            generation,
            failures: 0,
            task: None,
        });
        let delay = resubscribe_delay_ms(stream.failures);
        stream.failures += 1;
        stream.generation = generation;
        stream.task = Some(tokio_util::task::AbortOnDropHandle::new(tokio::spawn(
            async move {
                tokio::time::sleep(Duration::from_millis(delay)).await;
                let _ = sender
                    .send(Event::Resubscribe {
                        epoch,
                        key,
                        generation,
                    })
                    .await;
            },
        )));
    }

    pub(super) fn resubscribe(&mut self, key: StreamKey, generation: u64) {
        if !self.current(&key, generation) {
            return;
        }
        match key {
            StreamKey::Shell => self.subscribe_shell(ShellLocation::Active),
            StreamKey::Archive => self.subscribe_shell(ShellLocation::Archived),
            StreamKey::Thread(thread) => {
                if self.state.selected_thread.as_ref() == Some(&thread) {
                    self.subscribe_thread(&thread);
                }
            }
            StreamKey::Setup(thread) => {
                if self.state.selected_thread.as_ref() == Some(&thread) {
                    self.subscribe_setup(&thread);
                }
            }
            StreamKey::TerminalMetadata => self.subscribe_terminal_metadata(),
            StreamKey::Keybindings => self.subscribe_keybindings(),
            StreamKey::VcsStatus(cwd) => self.subscribe_vcs_status(cwd),
            StreamKey::GitAction(_) => {}
        }
    }

    pub(super) fn stream_item(&mut self, key: StreamKey, generation: u64, payload: Payload) {
        if !self.current(&key, generation) {
            return;
        }
        match (key, payload) {
            (key, Payload::Ended(error)) => self.stream_ended(key, error),
            (key, Payload::Shell(update)) => {
                let location = key.location().expect("shell stream");
                if matches!(update, ShellUpdate::Failed(_)) {
                    if let Some(shell) = self.shell_mut(location) {
                        shell.stream_error();
                    }
                    self.schedule_resubscribe(key);
                    return;
                }
                self.healthy(&key);
                self.shell_update(location, update);
            }
            (StreamKey::Thread(thread), Payload::Thread(update)) => {
                let failed = matches!(update, ThreadUpdate::Failed(_));
                if !failed {
                    self.healthy(&StreamKey::Thread(thread.clone()));
                }
                if self.thread_update(&thread, update) == Some(false) {
                    self.schedule_resubscribe(StreamKey::Thread(thread.clone()));
                }
            }
            (StreamKey::Setup(thread), Payload::Setup(setup)) => {
                self.healthy(&StreamKey::Setup(thread.clone()));
                self.setup_update(&thread, setup);
            }
            (StreamKey::TerminalMetadata, Payload::TerminalMetadata(event)) => {
                self.healthy(&StreamKey::TerminalMetadata);
                self.terminal_metadata(event);
            }
            (StreamKey::Keybindings, Payload::Keybindings(config)) => {
                self.healthy(&StreamKey::Keybindings);
                self.state.keybindings = Some(Arc::new(config));
            }
            (StreamKey::VcsStatus(cwd), Payload::VcsStatus(event)) => {
                self.healthy(&StreamKey::VcsStatus(cwd.clone()));
                self.state.git.apply_status(cwd, event);
            }
            (StreamKey::GitAction(action_id), Payload::ActionProgress(event)) => {
                let key = StreamKey::GitAction(action_id);
                let terminal = matches!(
                    &event.kind,
                    agent_protocol::vcs::ActionProgressKind::ActionFinished { .. }
                        | agent_protocol::vcs::ActionProgressKind::ActionFailed { .. }
                );
                self.healthy(&key);
                self.state.git.apply_action(event);
                if terminal {
                    self.close_stream(&key);
                }
            }
            _ => {}
        }
    }

    fn stream_ended(&mut self, key: StreamKey, error: Option<PeerError>) {
        let refused = error.as_ref().and_then(rpc_failure);
        if let Some(failure) = refused {
            match &key {
                StreamKey::Thread(thread) => {
                    let action = self
                        .state
                        .threads
                        .get_mut(thread)
                        .map(|sync| Arc::make_mut(sync).failed(&failure));
                    if action == Some(FailureAction::Deleted) {
                        self.thread_deleted(&thread.clone());
                        self.close_thread_streams(&thread.clone());
                        return;
                    }
                }
                StreamKey::Shell | StreamKey::Archive => {
                    if let Some(shell) = self.shell_mut(key.location().expect("shell stream")) {
                        shell.stream_error();
                    }
                }
                StreamKey::Setup(_)
                | StreamKey::TerminalMetadata
                | StreamKey::Keybindings
                | StreamKey::VcsStatus(_)
                | StreamKey::GitAction(_) => {}
            }
            self.schedule_resubscribe(key);
            return;
        }
        let closed = self
            .network
            .as_ref()
            .is_none_or(|network| network.peer.is_closed());
        if !closed {
            self.schedule_resubscribe(key);
        }
    }

    /// Applies one thread stream item. For a `Failed` item, returns whether the
    /// thread is gone (`Some(true)`) or should be subscribed again.
    pub(super) fn thread_update(
        &mut self,
        thread: &ThreadId,
        update: ThreadUpdate,
    ) -> Option<bool> {
        let failed = matches!(update, ThreadUpdate::Failed(_));
        let sync = self.state.threads.get_mut(thread)?;
        let applied = Arc::make_mut(sync).apply(vec![update]);
        let deleted = applied.deleted;
        self.thread_applied(thread, applied);
        failed.then_some(deleted)
    }

    pub(super) fn shell_update(&mut self, location: ShellLocation, update: ShellUpdate) {
        let epoch = self.epoch;
        let Some(shell) = self.shell_mut(location) else {
            return;
        };
        let applied = shell.apply(vec![update], epoch);
        if !applied.changed {
            return;
        }
        if location == ShellLocation::Active
            && let Some(snapshot) = &self.state.shell.snapshot
        {
            let revision = self.state.shell.revision;
            self.shell_cache.changed(revision, snapshot, now_ms());
        }
        self.complete_outbox();
        self.visit_selected();
        self.drain();
        if location == ShellLocation::Active {
            self.refresh_project_icons();
            self.ensure_selected_vcs_status();
        }
    }

    /// A resume dialog answered "Don't ask again" stops the offer for every
    /// thread of that provider instance, and the device keeps it.
    fn remember_resume_compaction_dismissal(&mut self, thread: &ThreadId) {
        let Some(state) = self.state.thread_state(thread) else {
            return;
        };
        if !crate::view::composer::context_meter::has_dismissed_resume_compaction(state) {
            return;
        }
        if let Some(instance) = state
            .thread
            .as_ref()
            .map(|thread| thread.selection.instance.clone())
        {
            self.state
                .preferences
                .resume_compaction_dismissed
                .insert(instance);
        }
    }

    fn thread_applied(&mut self, thread: &ThreadId, applied: Applied) {
        match applied.resync {
            Resync::Snapshot => self.subscribe_thread(thread),
            Resync::Stop => self.close_thread_streams(thread),
            Resync::None => {}
        }
        if applied.deleted {
            self.thread_deleted(thread);
            return;
        }
        if self.state.selected_thread.as_ref() == Some(thread) {
            self.ensure_selected_vcs_status();
        }
        if !applied.changed {
            return;
        }
        if let (Some(entry), Some(sync)) = (
            self.thread_caches.get_mut(thread),
            self.state.threads.get(thread),
        ) {
            entry.changed(sync, now_ms());
        }
        for result in applied.rollbacks {
            self.rollback_finished(thread, result);
        }
        self.remember_resume_compaction_dismissal(thread);
        self.end_finished_queue_edit(thread);
        self.complete_outbox();
        self.visit_selected();
        self.drain();
    }

    /// The thread is gone: its cache, drafts and selection go with it.
    pub(super) fn thread_deleted(&mut self, thread: &ThreadId) {
        if let Some(entry) = self.thread_caches.get_mut(thread) {
            entry.deleted();
        }
        if let Some(cache) = self.cache.clone() {
            let thread = thread.clone();
            self.write_file(move || {
                let _ = cache.remove_thread(&thread);
                None
            });
        }
        if let Some(sync) = self.state.threads.get_mut(thread)
            && sync.status != ThreadStatus::Deleted
        {
            Arc::make_mut(sync).set_deleted();
        }
        self.state_outbox().thread_deleted(thread);
        self.state.drafts.remove(thread.as_str());
        self.state.setups.remove(thread);
        self.state.held_setups.remove(thread);
        self.state.diff_panels.remove(thread);
        if self.state.selected_thread.as_ref() == Some(thread) {
            self.state.selected_thread = None;
            self.state.editing_run = None;
            self.close_thread_streams(thread);
        }
    }

    /// A queued message being edited started or left the queue.
    fn end_finished_queue_edit(&mut self, thread: &ThreadId) {
        if self.state.selected_thread.as_ref() != Some(thread) {
            return;
        }
        let Some(run) = self.state.editing_run.clone() else {
            return;
        };
        let queued = self.state.thread_state(thread).is_some_and(|state| {
            state
                .runs
                .iter()
                .any(|candidate| candidate.id == run && candidate.status == RunStatus::Queued)
        });
        if !queued {
            let key = self.state.draft_key();
            self.state.drafts.remove(&key);
            self.state.editing_run = None;
        }
    }

    /// After a rollback, the message of the first rolled-back run returns to
    /// the composer.
    fn rollback_finished(&mut self, thread: &ThreadId, result: RollbackResult) {
        let Some(pending) = self.state.rollbacks.remove(&result.command) else {
            return;
        };
        if !result.succeeded {
            return;
        }
        let Some(state) = self.state.thread_state(thread) else {
            return;
        };
        let Some(boundary) = state
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.id == pending.checkpoint)
            .map(|checkpoint| checkpoint.run_ordinal)
        else {
            return;
        };
        let restored = state
            .runs
            .iter()
            .filter(|run| run.status == RunStatus::RolledBack && run.ordinal > boundary)
            .min_by_key(|run| run.ordinal)
            .and_then(|run| state.message(&run.message))
            .map(|message| (message.text.clone(), message.attachments.clone()));
        let Some((text, attachments)) = restored else {
            return;
        };
        let mut draft = self.state.draft_for_thread(thread);
        draft.restore(&text, &attachments, None);
        self.state.drafts.insert(thread.to_string(), draft);
    }

    /// Marks the selected thread read up to its latest activity, once per watermark.
    pub(super) fn visit_selected(&mut self) {
        let Some(id) = self.state.selected_thread.clone() else {
            return;
        };
        let Some(row) = self.state.thread_row(&id) else {
            return;
        };
        let watermark = row
            .latest_run_completed_at
            .as_ref()
            .map_or(&row.updated_at, |completed| completed.max(&row.updated_at))
            .clone();
        if row
            .last_visited_at
            .as_ref()
            .is_some_and(|visited| visited >= &watermark)
            || self
                .visited
                .get(&id)
                .is_some_and(|visited| visited >= &watermark)
            || !self.connected()
        {
            return;
        }
        let command = lifecycle_command(LifecycleAction::Visit {
            at: watermark.clone(),
        });
        let entry = self.pending(id.clone(), command);
        if self.enqueue(entry, None).is_ok() {
            self.visited.insert(id, watermark);
        }
    }

    pub(super) fn load_earlier(&mut self) -> Result<(), PeerError> {
        let thread = self.selected()?;
        let Some(sync) = self.state.threads.get_mut(&thread) else {
            return Ok(());
        };
        let LoadEarlier::Request(cursor) = Arc::make_mut(sync).begin_load_earlier() else {
            return Ok(());
        };
        let sender = self.sender.clone();
        let Ok(network) = self.network() else {
            Arc::make_mut(self.state.threads.get_mut(&thread).expect("thread"))
                .history_failed(&cursor, NOT_CONNECTED);
            return Ok(());
        };
        let (peer, epoch) = (network.peer.clone(), network.epoch);
        network.spawn(async move {
            let result = peer
                .request::<HistoryPage>(&Call::ReadHistory(ReadHistory {
                    thread_id: thread.clone(),
                    cursor: Some(cursor.clone()),
                }))
                .await;
            let _ = sender
                .send(Event::History {
                    epoch,
                    thread,
                    cursor,
                    result,
                })
                .await;
        });
        Ok(())
    }

    pub(super) fn history_result(
        &mut self,
        thread: &ThreadId,
        cursor: &str,
        result: Result<HistoryPage, PeerError>,
    ) {
        let Some(sync) = self.state.threads.get_mut(thread) else {
            return;
        };
        let sync = Arc::make_mut(sync);
        match result {
            Ok(page) => {
                sync.history_loaded(cursor, page);
            }
            Err(error) => {
                let message = rpc_failure(&error).map_or_else(|| error.to_string(), |f| f.message);
                sync.history_failed(cursor, &message);
            }
        }
    }

    pub(super) fn load_detail(&mut self, item: TurnItemId) -> Result<(), PeerError> {
        let thread = self.selected()?;
        let sender = self.sender.clone();
        let network = self.network()?;
        let (peer, epoch) = (network.peer.clone(), network.epoch);
        let begun = self
            .state
            .threads
            .get_mut(&thread)
            .is_some_and(|sync| Arc::make_mut(sync).begin_detail(&item));
        if !begun {
            return Ok(());
        }
        self.network()?.spawn(async move {
            let result = peer
                .call(&GetTurnItem {
                    thread_id: thread.clone(),
                    item_id: item.clone(),
                })
                .await
                .map(|detail| detail.map(|detail| detail.row.item));
            let _ = sender
                .send(Event::Detail {
                    epoch,
                    thread,
                    item,
                    result: Box::new(result),
                })
                .await;
        });
        Ok(())
    }

    pub(super) fn detail_result(
        &mut self,
        thread: &ThreadId,
        item: &TurnItemId,
        result: Result<Option<Item>, PeerError>,
    ) {
        let Some(sync) = self.state.threads.get_mut(thread) else {
            return;
        };
        let sync = Arc::make_mut(sync);
        match result {
            Ok(loaded) => sync.detail_loaded(item, loaded),
            Err(error) => sync.detail_failed(item, error.to_string()),
        }
    }

    /// A closed setup stream keeps its last snapshot for the card.
    pub(super) fn setup_update(&mut self, thread: &ThreadId, setup: Option<WorktreeSetupSnapshot>) {
        match setup {
            Some(setup) => {
                self.state.held_setups.remove(thread);
                self.state.setups.insert(thread.clone(), setup);
            }
            None => {
                if let Some(setup) = self.state.setups.remove(thread) {
                    self.state.held_setups.insert(thread.clone(), setup);
                }
            }
        }
    }

    pub(super) fn selected(&self) -> Result<ThreadId, PeerError> {
        self.state
            .selected_thread
            .clone()
            .ok_or_else(|| super::invalid("Open a thread"))
    }

    pub(super) fn new_command_id(&self) -> agent_domain::CommandId {
        agent_domain::CommandId::new(new_id("command")).expect("valid id")
    }

    pub(super) fn now(&self) -> agent_domain::Timestamp {
        timestamp(now_ms())
    }
}
