use super::*;
use crate::client as rpc;

// One declaration owns each RPC Intent, typed response, ordering and state destination.
// Context is captured from the dispatch snapshot, never from the state at completion.
macro_rules! operation_table {
    ($define:ident, $snapshot:ident $(, $context:tt)*) => {
        $define! {
            $snapshot;
            loaded {
                ListAccounts => rpc::ListAccounts, rpc::ListAccounts {}, false;
                    (generation: u64 = $snapshot.account.accounts_generation) => accounts_loaded;
                SelectAccount(id: String) => rpc::SelectAccount<'static>, rpc::SelectAccount { account_id: &id }, false;
                    (generation: u64 = $snapshot.account.accounts_generation) => account_selected;
                StartAccountLogin => rpc::StartAccountLogin, rpc::StartAccountLogin {}, false;
                    (generation: u64 = $snapshot.account.login_generation) => account_login_started;
                ReadAccountLogin(id: String) => rpc::ReadAccountLogin<'static>, rpc::ReadAccountLogin { login_id: &id }, false;
                    (generation: u64 = $snapshot.account.login_generation) => account_login_updated;
                CancelAccountLogin(id: String) => rpc::CancelAccountLogin<'static>, rpc::CancelAccountLogin { login_id: &id }, false;
                    (generation: u64 = $snapshot.account.login_generation) => account_login_cancelled;
                ForkThread { thread_id: String, last_turn_id: String }
                    => rpc::ForkThread<'static>, rpc::ForkThread { thread_id: &thread_id, last_turn_id: &last_turn_id, exclude_turns: false }, true;
                    (generation: u64 = $snapshot.navigation.generation) => thread_forked
                    [value => Outcome::StartedThread(value.thread.id.clone().expect("validated thread ID"))];
                StartTerminal { handle: String, cwd: String, size: rpc::TerminalSize }
                    => rpc::StartTerminal<'static>, rpc::StartTerminal { process_handle: &handle, cwd: &cwd, size }, true;
                    (handle: String = handle) => terminal_started;
                CloseTerminal(handle: String) => rpc::KillTerminal<'static>, rpc::KillTerminal { process_handle: &handle }, true;
                    (handle: String = handle) => terminal_closed;
                Transcribe { draft_key: String, audio: String, send: bool, client_user_message_id: String }
                    => rpc::Transcribe<'static>, rpc::Transcribe { audio: &audio }, false;
                    (draft: Arc<Draft> = $snapshot.drafts.get(&draft_key).cloned().unwrap_or_default(),
                     draft_key: String = draft_key, generation: u64 = $snapshot.navigation.generation,
                     send: bool = send, client_user_message_id: String = client_user_message_id) => transcribed;
                CreateInvitation => rpc::CreateInvitation, rpc::CreateInvitation {}, false;
                    () => invitation_created;
                RemoveRemoteHost(id: String) => rpc::RemoveRemoteHost<'static>, rpc::RemoveRemoteHost { id: &id }, false;
                    (id: String = id) => remote_host_removed;
                RevokeDevice(id: String) => rpc::RevokeDevice<'static>, rpc::RevokeDevice { node_id: &id }, false;
                    (id: String = id) => device_revoked;
                OpenThread(thread_id: String)
                    => rpc::ReadThread<'static>, rpc::ReadThread { thread_id: &thread_id, include_turns: true, paginate_history: true, defer_item_details: true }, true;
                    (generation: u64 = $snapshot.navigation.generation) => thread_opened;
                ListFiles(path: String) => rpc::ListFiles<'static>, rpc::ListFiles { path: &path }, false;
                    (request: u64 = $snapshot.workspace.directory_request) => files_loaded;
                ReadFile { path: String, discard_draft: bool } prepare { let _ = discard_draft; }
                    => rpc::ReadFile<'static>, rpc::ReadFile { path: &path }, false;
                    (request: u64 = $snapshot.workspace.file_request) => file_loaded;
                SaveFile(path: String) prepare {
                    let submitted = $snapshot.file_drafts.get(&path).ok_or_else(||
                        PeerError::InvalidMessage("file has no draft to save".into()))?;
                }
                    => rpc::WriteFile<'static>, rpc::WriteFile { path: &path, revision: &submitted.revision, text: &submitted.text }, false;
                    (submitted: FileDraft = submitted.clone()) => file_saved;
                ReviewWorkspace(cwd: String) => rpc::ReviewWorkspace<'static>, rpc::ReviewWorkspace { cwd: &cwd }, false;
                    (request: u64 = $snapshot.workspace.review_request) => review_loaded;
                ReadWorktreeSettings => rpc::ReadWorktreeSettings, rpc::ReadWorktreeSettings {}, false;
                    (request: u64 = $snapshot.workspace.settings_request) => worktree_settings_loaded;
                UpdateWorktreeSettings(settings: WorktreeSettings)
                    => rpc::UpdateWorktreeSettings<'static>, rpc::UpdateWorktreeSettings(&settings), false;
                    (request: u64 = $snapshot.workspace.settings_request) => worktree_settings_loaded;
                ListThreads(query: ListQuery) => rpc::ListThreads<'static>, rpc::ListThreads { title_only: true, query: &query }, true;
                    (request: u64 = $snapshot.list_request) => threads_loaded;
                StartThread { cwd: Option<String>, model: Option<String> }
                    => rpc::StartThread<'static>, rpc::StartThread { cwd: cwd.as_deref().filter(|cwd| !cwd.trim().is_empty()), model: model.as_deref() }, true;
                    () => thread_read [value => Outcome::StartedThread(value.thread.id.clone().expect("validated thread ID"))];
                ReadThread(thread_id: String)
                    => rpc::ReadThread<'static>, rpc::ReadThread { thread_id: &thread_id, include_turns: true, paginate_history: true, defer_item_details: true }, true;
                    () => thread_read;
                ReadItem { thread_id: String, turn_id: String, item_id: String }
                    => rpc::ReadItem<'static>, rpc::ReadItem { thread_id: &thread_id, turn_id: &turn_id, item_id: &item_id }, true;
                    (thread_id: String = thread_id, turn_id: String = turn_id) => item_loaded;
            }
            ignored {
                ResizeTerminal { handle: String, size: rpc::TerminalSize } => rpc::ResizeTerminal { process_handle: &handle, size };
                Interrupt { thread_id: String, turn_id: String } => rpc::InterruptTurn { thread_id: &thread_id, turn_id: &turn_id };
                Watch { thread_id: String, watch_key: u64, watch_id: u64, path: Option<String> }
                    => rpc::WatchThread { thread_id: &thread_id, watch_key, watch_id, path: path.as_deref() };
                Unwatch { watch_key: u64, watch_id: u64 } => rpc::UnwatchThread { watch_key, watch_id };
            }
            other {
                ShowThreadList,
                WriteTerminal { handle: String, data: Vec<u8> },
                AcknowledgeTerminal { handle: String, sequence: u64 },
                AddAttachment { draft_key: String, attachment: Attachment },
                RemoveAttachment { draft_key: String, index: usize },
                UploadAttachment { draft_key: String, attachment: Attachment, directory: String },
                DownloadFile { source: std::path::PathBuf, destination: std::path::PathBuf },
                LoadSessionImages(String),
                LoadHostManagement,
                PairRemoteHost { invitation: Invitation, name: String },
                NewChat(String),
                SetFileDraft { path: String, text: String },
                ReadOlder { thread_id: String, turn_id: Option<String>, cursor: Option<String> },
                LoadModels,
                SetDraft { thread_id: String, draft: Draft },
                SetDraftText { thread_id: String, text: String },
                SelectModel { thread_id: String, model: String },
                SelectEffort { thread_id: String, effort: String },
                SelectServiceTier { thread_id: String, service_tier: String },
                Submit {
                    /// None submits the navigation target, creating its thread if needed.
                    thread_id: Option<String>, client_user_message_id: String,
                },
                Respond { request_id: Value, answer: Answer },
            }
            $($context)*
        }
    };
}
pub(crate) use operation_table;

macro_rules! define_operations {
    ($snapshot:ident;
     loaded { $(
        $variant:ident $(($($arg:ident: $ty:ty),* $(,)?))? $({$($field:ident: $field_ty:ty),* $(,)?})?
        $(prepare { $($prepare:tt)* })?
        => $operation:ty, $params:expr, $ordered:literal;
        ($($context:ident: $context_ty:ty = $capture:expr),* $(,)?) => $apply:ident
        $([$value:ident => $outcome:expr])?;
     )* }
     ignored { $(
        $ignored:ident $(($($iarg:ident: $ity:ty),* $(,)?))? $({$($ifield:ident: $ifield_ty:ty),* $(,)?})?
        => $ignore_params:expr;
     )* }
     other { $($other:tt)* }
    ) => {
        #[derive(Debug)]
        pub enum Intent {
            $($variant $(($($ty),*))? $({$($field: $field_ty),*})?,)*
            $($ignored $(($($ity),*))? $({$($ifield: $ifield_ty),*})?,)*
            $($other)*
        }
        /// A closed, typed sum of Operation outputs and their dispatch-time context.
        #[derive(Debug)]
        pub enum Loaded {
            $($variant { output: <$operation as rpc::Operation>::Output, $($context: $context_ty),* },)*
        }
        impl Loaded {
            pub(crate) fn reduce(self, previous: &Snapshot) -> (Snapshot, Vec<Effect>) {
                match self {
                    $(Self::$variant { output, $($context),* } => $apply(previous, output, $($context),*),)*
                }
            }
        }
    };
}
operation_table!(define_operations, snapshot);

fn terminal_started(
    previous: &Snapshot,
    _output: Map<String, Value>,
    handle: String,
) -> (Snapshot, Vec<Effect>) {
    let mut next = previous.clone();
    if let Some(terminal) = next.terminals.get(&handle)
        && terminal.phase == TerminalPhase::Starting
    {
        Arc::make_mut(Arc::make_mut(&mut next.terminals).get_mut(&handle).unwrap()).phase =
            TerminalPhase::Running;
    }
    (next, Vec::new())
}

fn terminal_closed(
    previous: &Snapshot,
    _output: Map<String, Value>,
    handle: String,
) -> (Snapshot, Vec<Effect>) {
    let mut next = previous.clone();
    if next.terminals.contains_key(&handle) {
        Arc::make_mut(Arc::make_mut(&mut next.terminals).get_mut(&handle).unwrap()).phase =
            TerminalPhase::Closed;
    }
    (next, Vec::new())
}

fn transcribed(
    previous: &Snapshot,
    output: rpc::Transcription,
    mut draft: Arc<Draft>,
    draft_key: String,
    generation: u64,
    send: bool,
    client_user_message_id: String,
) -> (Snapshot, Vec<Effect>) {
    let rpc::Transcription { text, .. } = output;
    let mut next = previous.clone();
    if send
        && previous.navigation.generation == generation
        && previous.navigation.draft_key == draft_key
    {
        let clear_draft = draft.clone();
        append_transcript(&mut Arc::make_mut(&mut draft).text, &text);
        let (mut next, effects) = submission(
            previous,
            previous.navigation.thread_id.clone(),
            draft_key,
            draft,
            client_user_message_id.clone(),
            Some(text),
        );
        Arc::make_mut(
            Arc::make_mut(&mut next.pending_submissions)
                .get_mut(&client_user_message_id)
                .unwrap(),
        )
        .clear_draft = Some(clear_draft);
        return (next, effects);
    }
    let draft = Arc::make_mut(
        Arc::make_mut(&mut next.drafts)
            .entry(draft_key)
            .or_default(),
    );
    append_transcript(&mut draft.text, &text);
    (next, Vec::new())
}

pub(super) fn add_attachment(next: &mut Snapshot, draft_key: String, attachment: Attachment) {
    Arc::make_mut(
        Arc::make_mut(&mut next.drafts)
            .entry(draft_key)
            .or_default(),
    )
    .attachments
    .push(attachment);
}

fn invitation_created(previous: &Snapshot, invitation: Invitation) -> (Snapshot, Vec<Effect>) {
    let mut next = previous.clone();
    Arc::make_mut(&mut next.management).invitation = Some(Arc::new(invitation));
    (next, Vec::new())
}

fn remote_host_removed(
    previous: &Snapshot,
    _output: Map<String, Value>,
    id: String,
) -> (Snapshot, Vec<Effect>) {
    let mut next = previous.clone();
    Arc::make_mut(&mut next.management)
        .remotes
        .retain(|host| host.id != id);
    (next, Vec::new())
}

fn device_revoked(
    previous: &Snapshot,
    _output: Map<String, Value>,
    id: String,
) -> (Snapshot, Vec<Effect>) {
    let mut next = previous.clone();
    if let Some(status) = Arc::make_mut(&mut next.management).status.as_mut() {
        Arc::make_mut(status).devices.retain(|device| device != &id);
    }
    (next, Vec::new())
}

fn thread_forked(
    previous: &Snapshot,
    output: crate::models::ThreadResponse,
    generation: u64,
) -> (Snapshot, Vec<Effect>) {
    let crate::models::ThreadResponse { thread, model, .. } = output;
    let (mut next, mut effects) = open_thread(previous, thread, model, generation);
    if previous.threads.is_some() {
        let (updated, refresh) = reduce(
            &next,
            Event::Intent(Intent::ListThreads((*previous.list_query).clone())),
        );
        next = updated;
        effects.extend(refresh);
    }
    (next, effects)
}

fn accounts_loaded(
    previous: &Snapshot,
    accounts: rpc::Accounts,
    generation: u64,
) -> (Snapshot, Vec<Effect>) {
    if generation != previous.account.accounts_generation {
        return (previous.clone(), Vec::new());
    }

    let mut next = previous.clone();
    Arc::make_mut(&mut next.account).accounts = Some(Arc::new(accounts));
    (next, Vec::new())
}

fn account_selected(
    previous: &Snapshot,
    output: rpc::AccountSelection,
    generation: u64,
) -> (Snapshot, Vec<Effect>) {
    if generation != previous.account.accounts_generation {
        return (previous.clone(), Vec::new());
    }
    let rpc::AccountSelection {
        selected_id,
        persistence_error,
        ..
    } = output;
    let mut next = previous.clone();
    if let Some(accounts) = &mut Arc::make_mut(&mut next.account).accounts {
        Arc::make_mut(accounts).selected_id = Some(selected_id);
    }
    next.error = persistence_error;
    (next, vec![Effect::Execute(Intent::LoadModels)])
}

fn account_login_started(
    previous: &Snapshot,
    login: rpc::AccountLogin,
    generation: u64,
) -> (Snapshot, Vec<Effect>) {
    if generation != previous.account.login_generation {
        return (previous.clone(), Vec::new());
    }

    let mut next = previous.clone();
    let account = Arc::make_mut(&mut next.account);
    account.login = Some(Arc::new(login));
    account.login_status = None;
    (next, Vec::new())
}

fn account_login_updated(
    previous: &Snapshot,
    status: rpc::AccountLoginStatus,
    generation: u64,
) -> (Snapshot, Vec<Effect>) {
    if generation != previous.account.login_generation {
        return (previous.clone(), Vec::new());
    }

    let mut next = previous.clone();
    let completed = status.completed;
    let account = Arc::make_mut(&mut next.account);
    account.login_status = Some(Arc::new(status));
    if completed {
        account.login = None;
        let (next, mut effects) = reduce(&next, Event::Intent(Intent::ListAccounts));
        effects.push(Effect::Execute(Intent::LoadModels));
        return (next, effects);
    }
    (next, Vec::new())
}

fn account_login_cancelled(
    previous: &Snapshot,
    _output: Map<String, Value>,
    generation: u64,
) -> (Snapshot, Vec<Effect>) {
    if generation != previous.account.login_generation {
        return (previous.clone(), Vec::new());
    }

    let mut next = previous.clone();
    let account = Arc::make_mut(&mut next.account);
    account.login = None;
    account.login_status = None;
    (next, Vec::new())
}

pub(super) fn open_thread(
    previous: &Snapshot,
    thread: Thread,
    model: Option<String>,
    generation: u64,
) -> (Snapshot, Vec<Effect>) {
    let id = thread.id.clone();
    let cwd = thread.cwd.clone().unwrap_or_default();
    let path = thread.path.clone();
    let (mut next, mut effects) = reduce(previous, Event::ThreadRefreshed(thread));
    if generation == previous.navigation.generation
        && let Some(id) = id
    {
        if previous.navigation.cwd != cwd {
            clear_workspace_location(Arc::make_mut(&mut next.workspace));
        }
        let navigation = Arc::make_mut(&mut next.navigation);
        if let Some(watch_id) = navigation.watch_id.take() {
            effects.push(Effect::Execute(Intent::Unwatch {
                watch_key: 1,
                watch_id,
            }));
        }
        navigation.watch_thread_id = None;
        navigation.thread_id = Some(id.clone());
        if previous.activity.unread.contains(&id) {
            Arc::make_mut(&mut next.activity).unread.remove(&id);
        }
        navigation.draft_key = id.clone();
        navigation.cwd = cwd;
        if path.is_some() {
            navigation.watch_id = Some(generation);
            navigation.watch_thread_id = Some(id.clone());
            effects.push(Effect::Execute(Intent::Watch {
                thread_id: id.clone(),
                watch_key: 1,
                watch_id: generation,
                path,
            }));
        }
        if let Some(model) = model {
            let (updated, _) = reduce(
                &next,
                Event::Intent(Intent::SelectModel {
                    thread_id: id,
                    model,
                }),
            );
            next = updated;
        }
    }
    (next, effects)
}

fn thread_opened(
    previous: &Snapshot,
    output: crate::models::ThreadResponse,
    generation: u64,
) -> (Snapshot, Vec<Effect>) {
    open_thread(previous, output.thread, output.model, generation)
}

fn files_loaded(previous: &Snapshot, files: FileList, request: u64) -> (Snapshot, Vec<Effect>) {
    let mut next = previous.clone();
    if request == previous.workspace.directory_request {
        Arc::make_mut(&mut next.workspace).directory = Some(Arc::new(files));
    }
    (next, Vec::new())
}

fn file_loaded(previous: &Snapshot, file: FileContent, request: u64) -> (Snapshot, Vec<Effect>) {
    let mut next = previous.clone();
    if request == previous.workspace.file_request {
        Arc::make_mut(&mut next.workspace).file = Some(Arc::new(file));
    }
    (next, Vec::new())
}

fn file_saved(
    previous: &Snapshot,
    file: FileContent,
    submitted: FileDraft,
) -> (Snapshot, Vec<Effect>) {
    let mut next = previous.clone();
    if let Some(current) = previous.file_drafts.get(&file.path) {
        if current == &submitted {
            Arc::make_mut(&mut next.file_drafts).remove(&file.path);
        } else if current.revision == submitted.revision {
            Arc::make_mut(&mut next.file_drafts)
                .get_mut(&file.path)
                .unwrap()
                .revision = file.revision.clone();
        }
    }
    if previous
        .workspace
        .file
        .as_ref()
        .is_some_and(|current| current.path == file.path)
    {
        Arc::make_mut(&mut next.workspace).file = Some(Arc::new(file));
    }
    (next, Vec::new())
}

fn review_loaded(
    previous: &Snapshot,
    review: WorkspaceReview,
    request: u64,
) -> (Snapshot, Vec<Effect>) {
    let mut next = previous.clone();
    if request == previous.workspace.review_request {
        Arc::make_mut(&mut next.workspace).review = Some(Arc::new(review));
    }
    (next, Vec::new())
}

fn worktree_settings_loaded(
    previous: &Snapshot,
    settings: WorktreeSettings,
    request: u64,
) -> (Snapshot, Vec<Effect>) {
    let mut next = previous.clone();
    if request == previous.workspace.settings_request {
        Arc::make_mut(&mut next.workspace).settings = Some(Arc::new(settings));
    }
    (next, Vec::new())
}

fn item_loaded(
    previous: &Snapshot,
    output: rpc::ItemResponse,
    thread_id: String,
    turn_id: String,
) -> (Snapshot, Vec<Effect>) {
    let rpc::ItemResponse { item, .. } = output;
    let mut next = upsert_item(previous, &thread_id, &turn_id, item);
    reconcile_pending(&mut next, &thread_id);
    (next, Vec::new())
}

fn threads_loaded(
    previous: &Snapshot,
    threads: ThreadList,
    request: u64,
) -> (Snapshot, Vec<Effect>) {
    let mut next = previous.clone();
    if request == previous.list_request {
        next.threads = Some(Arc::new(threads));
    }
    (next, Vec::new())
}

fn thread_read(
    previous: &Snapshot,
    output: crate::models::ThreadResponse,
) -> (Snapshot, Vec<Effect>) {
    reduce(previous, Event::ThreadRefreshed(output.thread))
}
