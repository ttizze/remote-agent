use super::*;
use crate::{client as rpc, store::Outcome};

#[derive(Debug)]
pub enum Intent {
    ListAccounts,
    SelectAccount(String),
    StartAccountLogin,
    ReadAccountLogin(String),
    CancelAccountLogin(String),
    ForkThread {
        thread_id: String,
        last_turn_id: String,
    },
    StartTerminal {
        handle: String,
        cwd: String,
        size: rpc::TerminalSize,
    },
    CloseTerminal(String),
    Transcribe {
        draft_key: String,
        audio: String,
        send: bool,
        client_user_message_id: String,
    },
    CreateInvitation,
    RemoveRemoteHost(String),
    RevokeDevice(String),
    OpenThread(String),
    ListFiles(String),
    ReadFile {
        path: String,
        discard_draft: bool,
    },
    SaveFile(String),
    ReviewWorkspace(String),
    ReadWorktreeSettings,
    UpdateWorktreeSettings(WorktreeSettings),
    ListThreads(ListQuery),
    StartThread {
        cwd: Option<String>,
        model: Option<String>,
    },
    ReadThread(String),
    ReadItem {
        thread_id: String,
        turn_id: String,
        item_id: String,
    },
    ResizeTerminal {
        handle: String,
        size: rpc::TerminalSize,
    },
    Interrupt {
        thread_id: String,
        turn_id: String,
    },
    Watch {
        thread_id: String,
        watch_key: u64,
        watch_id: u64,
        path: Option<String>,
    },
    Unwatch {
        watch_key: u64,
        watch_id: u64,
    },
    ShowThreadList,
    WriteTerminal {
        handle: String,
        data: Vec<u8>,
    },
    AcknowledgeTerminal {
        handle: String,
        sequence: u64,
    },
    AddAttachment {
        draft_key: String,
        attachment: Attachment,
    },
    RemoveAttachment {
        draft_key: String,
        index: usize,
    },
    UploadAttachment {
        draft_key: String,
        attachment: Attachment,
        directory: String,
    },
    DownloadFile {
        source: std::path::PathBuf,
        destination: std::path::PathBuf,
    },
    LoadSessionImages(String),
    LoadHostManagement,
    PairRemoteHost {
        invitation: Invitation,
        name: String,
    },
    NewChat(String),
    SetFileDraft {
        path: String,
        text: String,
    },
    ReadOlder {
        thread_id: String,
        turn_id: Option<String>,
        cursor: Option<String>,
    },
    LoadModels,
    SetDraft {
        thread_id: String,
        draft: Draft,
    },
    SetDraftText {
        thread_id: String,
        text: String,
    },
    SelectModel {
        thread_id: String,
        model: String,
    },
    SelectEffort {
        thread_id: String,
        effort: String,
    },
    SelectServiceTier {
        thread_id: String,
        service_tier: String,
    },
    Submit {
        /// None submits the navigation target, creating its thread if needed.
        thread_id: Option<String>,
        client_user_message_id: String,
    },
    Respond {
        request_id: Value,
        answer: Answer,
    },
}

/// Typed state application after the Store has checked its single epoch.
/// Only operations with durable side effects override `stale`.
pub trait Operation: Send + Sync + std::fmt::Debug + Sized + 'static {
    type Output: Send + std::fmt::Debug + 'static;
    const ORDERED: bool = false;
    fn request(&self) -> impl rpc::Operation<Output = Self::Output> + Sync;
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect>;
    fn stale(self, _snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        Vec::new()
    }
    fn outcome(_output: &Self::Output) -> Outcome {
        Outcome::Applied
    }
}

// Heterogeneous RPC outputs need one owned allocation while awaiting wire order.
// The output remains typed; no JSON round trip or second response enum is needed.
pub trait Application: Send + std::fmt::Debug {
    fn apply(self: Box<Self>, snapshot: &mut Snapshot, current: bool) -> Vec<Effect>;
}
#[derive(Debug)]
struct Completion<O: Operation> {
    operation: O,
    output: O::Output,
}
impl<O: Operation> Application for Completion<O> {
    fn apply(self: Box<Self>, snapshot: &mut Snapshot, current: bool) -> Vec<Effect> {
        if current {
            self.operation.apply(snapshot, self.output)
        } else {
            self.operation.stale(snapshot, self.output)
        }
    }
}
pub fn completed<O: Operation>(operation: O, output: O::Output) -> Event {
    Event::Operation(Box::new(Completion { operation, output }))
}

#[derive(Debug)]
pub struct StartTerminal {
    pub handle: String,
    pub cwd: String,
    pub size: rpc::TerminalSize,
}
impl Operation for StartTerminal {
    type Output = Map<String, Value>;
    const ORDERED: bool = true;
    fn request(&self) -> impl rpc::Operation<Output = Self::Output> + Sync {
        rpc::StartTerminal {
            process_handle: &self.handle,
            cwd: &self.cwd,
            size: self.size,
        }
    }
    fn apply(self, snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        let Self { handle, .. } = self;
        if let Some(terminal) = snapshot.terminals.get(&handle)
            && terminal.phase == TerminalPhase::Starting
        {
            Arc::make_mut(
                Arc::make_mut(&mut snapshot.terminals)
                    .get_mut(&handle)
                    .unwrap(),
            )
            .phase = TerminalPhase::Running;
        }
        Vec::new()
    }
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        self.apply(snapshot, output)
    }
}

#[derive(Debug)]
pub struct CloseTerminal {
    pub handle: String,
}
impl Operation for CloseTerminal {
    type Output = Map<String, Value>;
    const ORDERED: bool = true;
    fn request(&self) -> impl rpc::Operation<Output = Self::Output> + Sync {
        rpc::KillTerminal {
            process_handle: &self.handle,
        }
    }
    fn apply(self, snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        let Self { handle, .. } = self;
        if snapshot.terminals.contains_key(&handle) {
            Arc::make_mut(
                Arc::make_mut(&mut snapshot.terminals)
                    .get_mut(&handle)
                    .unwrap(),
            )
            .phase = TerminalPhase::Closed;
        }
        Vec::new()
    }
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        self.apply(snapshot, output)
    }
}

#[derive(Debug)]
pub struct CreateInvitation;
impl Operation for CreateInvitation {
    type Output = Invitation;
    fn request(&self) -> impl rpc::Operation<Output = Self::Output> + Sync {
        rpc::CreateInvitation {}
    }
    fn apply(self, snapshot: &mut Snapshot, invitation: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.management).invitation = Some(Arc::new(invitation));
        Vec::new()
    }
}

#[derive(Debug)]
pub struct RemoveRemoteHost {
    pub id: String,
}
impl Operation for RemoveRemoteHost {
    type Output = Map<String, Value>;
    fn request(&self) -> impl rpc::Operation<Output = Self::Output> + Sync {
        rpc::RemoveRemoteHost { id: &self.id }
    }
    fn apply(self, snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        let Self { id } = self;
        Arc::make_mut(&mut snapshot.management)
            .remotes
            .retain(|host| host.id != id);
        Vec::new()
    }
}

#[derive(Debug)]
pub struct RevokeDevice {
    pub id: String,
}
impl Operation for RevokeDevice {
    type Output = Map<String, Value>;
    fn request(&self) -> impl rpc::Operation<Output = Self::Output> + Sync {
        rpc::RevokeDevice { node_id: &self.id }
    }
    fn apply(self, snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        let Self { id } = self;
        if let Some(status) = Arc::make_mut(&mut snapshot.management).status.as_mut() {
            Arc::make_mut(status).devices.retain(|device| device != &id);
        }
        Vec::new()
    }
}

#[derive(Debug)]
pub struct ListAccounts;
impl Operation for ListAccounts {
    type Output = rpc::Accounts;
    fn request(&self) -> impl rpc::Operation<Output = Self::Output> + Sync {
        rpc::ListAccounts {}
    }
    fn apply(self, snapshot: &mut Snapshot, accounts: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.account).accounts = Some(Arc::new(accounts));
        Vec::new()
    }
}

#[derive(Debug)]
pub struct SelectAccount {
    pub id: String,
}
impl Operation for SelectAccount {
    type Output = rpc::AccountSelection;
    fn request(&self) -> impl rpc::Operation<Output = Self::Output> + Sync {
        rpc::SelectAccount {
            account_id: &self.id,
        }
    }
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        let rpc::AccountSelection {
            selected_id,
            persistence_error,
            ..
        } = output;
        if let Some(accounts) = &mut Arc::make_mut(&mut snapshot.account).accounts {
            Arc::make_mut(accounts).selected_id = Some(selected_id);
        }
        snapshot.error = persistence_error;
        vec![Effect::Execute(Intent::LoadModels)]
    }
}

#[derive(Debug)]
pub struct StartAccountLogin;
impl Operation for StartAccountLogin {
    type Output = rpc::AccountLogin;
    fn request(&self) -> impl rpc::Operation<Output = Self::Output> + Sync {
        rpc::StartAccountLogin {}
    }
    fn apply(self, snapshot: &mut Snapshot, login: Self::Output) -> Vec<Effect> {
        let account = Arc::make_mut(&mut snapshot.account);
        account.login = Some(Arc::new(login));
        account.login_status = None;
        Vec::new()
    }
}

#[derive(Debug)]
pub struct ReadAccountLogin {
    pub id: String,
}
impl Operation for ReadAccountLogin {
    type Output = rpc::AccountLoginStatus;
    fn request(&self) -> impl rpc::Operation<Output = Self::Output> + Sync {
        rpc::ReadAccountLogin { login_id: &self.id }
    }
    fn apply(self, snapshot: &mut Snapshot, status: Self::Output) -> Vec<Effect> {
        let completed = status.completed;
        let account = Arc::make_mut(&mut snapshot.account);
        account.login_status = Some(Arc::new(status));
        if completed {
            account.login = None;
            let (updated, mut effects) = reduce(snapshot, Event::Intent(Intent::ListAccounts));
            *snapshot = updated;
            effects.push(Effect::Execute(Intent::LoadModels));
            return effects;
        }
        Vec::new()
    }
}

#[derive(Debug)]
pub struct CancelAccountLogin {
    pub id: String,
}
impl Operation for CancelAccountLogin {
    type Output = Map<String, Value>;
    fn request(&self) -> impl rpc::Operation<Output = Self::Output> + Sync {
        rpc::CancelAccountLogin { login_id: &self.id }
    }
    fn apply(self, snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        let account = Arc::make_mut(&mut snapshot.account);
        account.login = None;
        account.login_status = None;
        Vec::new()
    }
}

#[derive(Debug)]
pub struct ListFiles {
    pub path: String,
}
impl Operation for ListFiles {
    type Output = FileList;
    fn request(&self) -> impl rpc::Operation<Output = Self::Output> + Sync {
        rpc::ListFiles { path: &self.path }
    }
    fn apply(self, snapshot: &mut Snapshot, files: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.workspace).directory = Some(Arc::new(files));
        Vec::new()
    }
}

#[derive(Debug)]
pub struct ReadFile {
    pub path: String,
}
impl Operation for ReadFile {
    type Output = FileContent;
    fn request(&self) -> impl rpc::Operation<Output = Self::Output> + Sync {
        rpc::ReadFile { path: &self.path }
    }
    fn apply(self, snapshot: &mut Snapshot, file: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.workspace).file = Some(Arc::new(file));
        Vec::new()
    }
}

#[derive(Debug)]
pub struct SaveFile {
    pub path: String,
    pub submitted: FileDraft,
}
impl Operation for SaveFile {
    type Output = FileContent;
    fn request(&self) -> impl rpc::Operation<Output = Self::Output> + Sync {
        rpc::WriteFile {
            path: &self.path,
            revision: &self.submitted.revision,
            text: &self.submitted.text,
        }
    }
    fn apply(self, snapshot: &mut Snapshot, file: Self::Output) -> Vec<Effect> {
        self.rebase_draft(snapshot, &file);
        if snapshot
            .workspace
            .file
            .as_ref()
            .is_some_and(|current| current.path == file.path)
        {
            Arc::make_mut(&mut snapshot.workspace).file = Some(Arc::new(file));
        }
        Vec::new()
    }
    fn stale(self, snapshot: &mut Snapshot, file: Self::Output) -> Vec<Effect> {
        self.rebase_draft(snapshot, &file);
        Vec::new()
    }
}
impl SaveFile {
    fn rebase_draft(self, snapshot: &mut Snapshot, file: &FileContent) {
        let Self { submitted, .. } = self;
        if let Some(current) = snapshot.file_drafts.get(&file.path) {
            if current == &submitted {
                Arc::make_mut(&mut snapshot.file_drafts).remove(&file.path);
            } else if current.revision == submitted.revision {
                Arc::make_mut(&mut snapshot.file_drafts)
                    .get_mut(&file.path)
                    .unwrap()
                    .revision = file.revision.clone();
            }
        }
    }
}

#[derive(Debug)]
pub struct ReviewWorkspace {
    pub cwd: String,
}
impl Operation for ReviewWorkspace {
    type Output = WorkspaceReview;
    fn request(&self) -> impl rpc::Operation<Output = Self::Output> + Sync {
        rpc::ReviewWorkspace { cwd: &self.cwd }
    }
    fn apply(self, snapshot: &mut Snapshot, review: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.workspace).review = Some(Arc::new(review));
        Vec::new()
    }
}

#[derive(Debug)]
pub struct ReadWorktreeSettings;
impl Operation for ReadWorktreeSettings {
    type Output = super::WorktreeSettings;
    fn request(&self) -> impl rpc::Operation<Output = Self::Output> + Sync {
        rpc::ReadWorktreeSettings {}
    }
    fn apply(self, snapshot: &mut Snapshot, settings: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.workspace).settings = Some(Arc::new(settings));
        Vec::new()
    }
}

#[derive(Debug)]
pub struct UpdateWorktreeSettings {
    pub settings: super::WorktreeSettings,
}
impl Operation for UpdateWorktreeSettings {
    type Output = super::WorktreeSettings;
    fn request(&self) -> impl rpc::Operation<Output = Self::Output> + Sync {
        rpc::UpdateWorktreeSettings(&self.settings)
    }
    fn apply(self, snapshot: &mut Snapshot, settings: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.workspace).settings = Some(Arc::new(settings));
        Vec::new()
    }
}

#[derive(Debug)]
pub struct ListThreads {
    pub query: ListQuery,
}
impl Operation for ListThreads {
    type Output = ThreadList;
    const ORDERED: bool = true;
    fn request(&self) -> impl rpc::Operation<Output = Self::Output> + Sync {
        rpc::ListThreads {
            title_only: true,
            query: &self.query,
        }
    }
    fn apply(self, snapshot: &mut Snapshot, threads: Self::Output) -> Vec<Effect> {
        snapshot.threads = Some(Arc::new(threads));
        Vec::new()
    }
}

#[derive(Debug)]
pub struct ReadItem {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
}
impl Operation for ReadItem {
    type Output = rpc::ItemResponse;
    const ORDERED: bool = true;
    fn request(&self) -> impl rpc::Operation<Output = Self::Output> + Sync {
        rpc::ReadItem {
            thread_id: &self.thread_id,
            turn_id: &self.turn_id,
            item_id: &self.item_id,
        }
    }
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        let Self {
            thread_id, turn_id, ..
        } = self;
        let rpc::ItemResponse { item, .. } = output;
        *snapshot = upsert_item(snapshot, &thread_id, &turn_id, item);
        reconcile_pending(snapshot, &thread_id);
        Vec::new()
    }
}

#[derive(Debug)]
pub struct ReadThread {
    pub thread_id: String,
}
impl Operation for ReadThread {
    type Output = crate::models::ThreadResponse;
    const ORDERED: bool = true;
    fn request(&self) -> impl rpc::Operation<Output = Self::Output> + Sync {
        rpc::ReadThread {
            thread_id: &self.thread_id,
            include_turns: true,
            paginate_history: true,
            defer_item_details: true,
        }
    }
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        let (updated, effects) = reduce(snapshot, Event::ThreadRefreshed(output.thread));
        *snapshot = updated;
        effects
    }
}

pub(super) fn open_thread(
    previous: &Snapshot,
    thread: Thread,
    model: Option<String>,
) -> (Snapshot, Vec<Effect>) {
    let id = thread.id.clone();
    let cwd = thread.cwd.clone().unwrap_or_default();
    let path = thread.path.clone();
    let (mut next, mut effects) = reduce(previous, Event::ThreadRefreshed(thread));
    if let Some(id) = id {
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
            navigation.watch_id = Some(previous.epoch);
            navigation.watch_thread_id = Some(id.clone());
            effects.push(Effect::Execute(Intent::Watch {
                thread_id: id.clone(),
                watch_key: 1,
                watch_id: previous.epoch,
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

pub(super) fn add_attachment(next: &mut Snapshot, draft_key: String, attachment: Attachment) {
    Arc::make_mut(
        Arc::make_mut(&mut next.drafts)
            .entry(draft_key)
            .or_default(),
    )
    .attachments
    .push(attachment);
}

#[derive(Debug)]
pub struct OpenThread {
    pub thread_id: String,
}
impl Operation for OpenThread {
    type Output = crate::models::ThreadResponse;
    const ORDERED: bool = true;
    fn request(&self) -> impl rpc::Operation<Output = Self::Output> + Sync {
        rpc::ReadThread {
            thread_id: &self.thread_id,
            include_turns: true,
            paginate_history: true,
            defer_item_details: true,
        }
    }
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        let (next, effects) = open_thread(snapshot, output.thread, output.model);
        *snapshot = next;
        effects
    }
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        let (next, effects) = reduce(snapshot, Event::ThreadRefreshed(output.thread));
        *snapshot = next;
        effects
    }
}
#[derive(Debug)]
pub struct ForkThread {
    pub thread_id: String,
    pub last_turn_id: String,
}
impl Operation for ForkThread {
    type Output = crate::models::ThreadResponse;
    const ORDERED: bool = true;
    fn request(&self) -> impl rpc::Operation<Output = Self::Output> + Sync {
        rpc::ForkThread {
            thread_id: &self.thread_id,
            last_turn_id: &self.last_turn_id,
            exclude_turns: false,
        }
    }
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        let (next, mut effects) = open_thread(snapshot, output.thread, output.model);
        *snapshot = next;
        if snapshot.threads.is_some() {
            effects.push(Effect::Execute(Intent::ListThreads(
                (*snapshot.list_query).clone(),
            )));
        }
        effects
    }
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        let (next, effects) = reduce(snapshot, Event::ThreadRefreshed(output.thread));
        *snapshot = next;
        effects
    }
    fn outcome(output: &Self::Output) -> Outcome {
        Outcome::StartedThread(output.thread.id.clone().expect("validated thread ID"))
    }
}
#[derive(Debug)]
pub struct StartThread {
    pub cwd: Option<String>,
    pub model: Option<String>,
}
impl Operation for StartThread {
    type Output = crate::models::ThreadResponse;
    const ORDERED: bool = true;
    fn request(&self) -> impl rpc::Operation<Output = Self::Output> + Sync {
        rpc::StartThread {
            cwd: self.cwd.as_deref().filter(|cwd| !cwd.trim().is_empty()),
            model: self.model.as_deref(),
        }
    }
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        let (next, effects) = reduce(snapshot, Event::ThreadRefreshed(output.thread));
        *snapshot = next;
        effects
    }
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        self.apply(snapshot, output)
    }
    fn outcome(output: &Self::Output) -> Outcome {
        ForkThread::outcome(output)
    }
}
#[derive(Debug)]
pub struct Transcribe {
    pub draft: Arc<Draft>,
    pub draft_key: String,
    pub audio: String,
    pub send: bool,
    pub client_user_message_id: String,
}
impl Operation for Transcribe {
    type Output = rpc::Transcription;
    fn request(&self) -> impl rpc::Operation<Output = Self::Output> + Sync {
        rpc::Transcribe { audio: &self.audio }
    }
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        if !self.send {
            return self.stale(snapshot, output);
        }
        let Self {
            mut draft,
            draft_key,
            client_user_message_id,
            ..
        } = self;
        let clear_draft = draft.clone();
        append_transcript(&mut Arc::make_mut(&mut draft).text, &output.text);
        let (mut next, effects) = submission(
            snapshot,
            snapshot.navigation.thread_id.clone(),
            draft_key,
            draft,
            client_user_message_id.clone(),
            Some(output.text),
        );
        Arc::make_mut(
            Arc::make_mut(&mut next.pending_submissions)
                .get_mut(&client_user_message_id)
                .unwrap(),
        )
        .clear_draft = Some(clear_draft);
        *snapshot = next;
        effects
    }
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        let draft = Arc::make_mut(
            Arc::make_mut(&mut snapshot.drafts)
                .entry(self.draft_key)
                .or_default(),
        );
        append_transcript(&mut draft.text, &output.text);
        Vec::new()
    }
}
