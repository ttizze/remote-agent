use super::*;
use crate::{
    client as rpc,
    peer::PeerError,
    store::{Execution, Outcome},
};
use rpc::{Input, Submission, submission_target};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum Intent {
    ListAccounts(ListAccounts),
    SelectAccount(SelectAccount),
    StartAccountLogin(StartAccountLogin),
    ReadAccountLogin(ReadAccountLogin),
    CancelAccountLogin(CancelAccountLogin),
    ForkThread(ForkThread),
    StartTerminal(StartTerminal),
    CloseTerminal(CloseTerminal),
    Transcribe(Dictate),
    CreateInvitation(CreateInvitation),
    RemoveRemoteHost(RemoveRemoteHost),
    RevokeDevice(RevokeDevice),
    ListFiles(ListFiles),
    ReadFile(ReadFile),
    SaveFile(SaveFile),
    ReviewWorkspace(ReviewWorkspace),
    ReadWorktreeSettings(ReadWorktreeSettings),
    UpdateWorktreeSettings(UpdateWorktreeSettings),
    ListThreads(ListThreads),
    StartThread(StartThread),
    ReadThread(ReadThread),
    ReadItem(ReadItem),
    ResizeTerminal(ResizeTerminal),
    Interrupt(Interrupt),
    Watch(Watch),
    Unwatch(Unwatch),
    ShowThreadList,
    WriteTerminal(WriteTerminal),
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
        index: u32,
    },
    UploadAttachment(UploadAttachment),
    DownloadFile(DownloadFile),
    LoadSessionImages(LoadSessionImages),
    LoadHostManagement(LoadHostManagement),
    PairRemoteHost(PairRemoteHost),
    NewChat {
        cwd: String,
    },
    SetFileDraft {
        path: String,
        text: String,
    },
    ReadOlder(ReadOlder),
    LoadModels(LoadModels),
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
        thread_id: Option<String>,
        client_user_message_id: String,
    },
    Respond(Respond),
}

/// Typed state application after the Store has checked its single epoch.
/// Only operations with durable side effects override `stale`.
pub trait Operation: Send + Sync + std::fmt::Debug + Sized + 'static {
    type Output: Send + std::fmt::Debug + 'static;
    const ORDERED: bool = false;
    fn terminal_handle(&self) -> Option<&str> {
        None
    }
    fn disconnected_is_complete(&self) -> bool {
        false
    }
    fn run(
        &self,
        context: &mut Execution<'_>,
    ) -> impl Future<Output = Result<Self::Output, PeerError>> + Send;
    fn invalidates(&self, _snapshot: &Snapshot) -> bool {
        false
    }
    fn prepare(&self, _snapshot: &mut Snapshot) -> Result<(), String> {
        Ok(())
    }
    fn apply(self, _snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        Vec::new()
    }
    fn stale(self, _snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        Vec::new()
    }
    fn outcome(_output: &mut Self::Output) -> Outcome {
        Outcome::Applied
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartTerminal {
    #[serde(rename = "processHandle")]
    pub handle: String,
    pub cwd: String,
    pub size: rpc::TerminalSize,
}
impl rpc::RpcMethod for StartTerminal {
    type Output = Map<String, Value>;
    const METHOD: &'static str = "host/terminal/start";
}

impl Operation for StartTerminal {
    fn terminal_handle(&self) -> Option<&str> {
        Some(&self.handle)
    }
    fn prepare(&self, snapshot: &mut Snapshot) -> Result<(), String> {
        if snapshot.terminals.contains_key(&self.handle) {
            return Err("terminal handle is already in use".into());
        }
        Arc::make_mut(&mut snapshot.terminals).insert(
            self.handle.clone(),
            Arc::new(Terminal {
                cwd: self.cwd.clone(),
                phase: TerminalPhase::Starting,
                output: VecDeque::new(),
                sequence: 0,
            }),
        );
        Ok(())
    }
    type Output = Map<String, Value>;
    const ORDERED: bool = true;
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.call(self).await
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

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloseTerminal {
    #[serde(rename = "processHandle")]
    pub handle: String,
}
impl rpc::RpcMethod for CloseTerminal {
    type Output = Map<String, Value>;
    const METHOD: &'static str = "process/kill";
}

impl Operation for CloseTerminal {
    fn terminal_handle(&self) -> Option<&str> {
        Some(&self.handle)
    }
    type Output = Map<String, Value>;
    const ORDERED: bool = true;
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.call(self).await
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

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateInvitation {}
impl rpc::RpcMethod for CreateInvitation {
    type Output = Invitation;
    const METHOD: &'static str = "host/invite";
}

impl Operation for CreateInvitation {
    type Output = Invitation;
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.call(self).await
    }
    fn apply(self, snapshot: &mut Snapshot, invitation: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.management).invitation = Some(Arc::new(invitation));
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoveRemoteHost {
    pub id: String,
}
impl rpc::RpcMethod for RemoveRemoteHost {
    type Output = Map<String, Value>;
    const METHOD: &'static str = "host/removeRemote";
}

impl Operation for RemoveRemoteHost {
    type Output = Map<String, Value>;
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.call(self).await
    }
    fn apply(self, snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        let Self { id } = self;
        Arc::make_mut(&mut snapshot.management)
            .remotes
            .retain(|host| host.id != id);
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevokeDevice {
    #[serde(rename = "nodeId")]
    pub id: String,
}
impl rpc::RpcMethod for RevokeDevice {
    type Output = Map<String, Value>;
    const METHOD: &'static str = "host/revoke";
}

impl Operation for RevokeDevice {
    type Output = Map<String, Value>;
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.call(self).await
    }
    fn apply(self, snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        let Self { id } = self;
        if let Some(status) = Arc::make_mut(&mut snapshot.management).status.as_mut() {
            Arc::make_mut(status).devices.retain(|device| device != &id);
        }
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListAccounts {}
impl rpc::RpcMethod for ListAccounts {
    type Output = rpc::Accounts;
    const METHOD: &'static str = "host/account/list";
}

impl Operation for ListAccounts {
    type Output = rpc::Accounts;
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.call(self).await
    }
    fn apply(self, snapshot: &mut Snapshot, accounts: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.account).accounts = Some(Arc::new(accounts));
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectAccount {
    #[serde(rename = "accountId")]
    pub id: String,
}
impl rpc::RpcMethod for SelectAccount {
    type Output = rpc::AccountSelection;
    const METHOD: &'static str = "host/account/select";
}

impl Operation for SelectAccount {
    fn invalidates(&self, _snapshot: &Snapshot) -> bool {
        true
    }
    type Output = rpc::AccountSelection;
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.call(self).await
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
        vec![Effect::execute(LoadModels {})]
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartAccountLogin {}
impl rpc::RpcMethod for StartAccountLogin {
    type Output = rpc::AccountLogin;
    const METHOD: &'static str = "host/account/login/start";
}

impl Operation for StartAccountLogin {
    fn invalidates(&self, _snapshot: &Snapshot) -> bool {
        true
    }
    type Output = rpc::AccountLogin;
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.call(self).await
    }
    fn apply(self, snapshot: &mut Snapshot, login: Self::Output) -> Vec<Effect> {
        let account = Arc::make_mut(&mut snapshot.account);
        account.login = Some(Arc::new(login));
        account.login_status = None;
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadAccountLogin {
    #[serde(rename = "loginId")]
    pub id: String,
}
impl rpc::RpcMethod for ReadAccountLogin {
    type Output = rpc::AccountLoginStatus;
    const METHOD: &'static str = "host/account/login/status";
}

impl Operation for ReadAccountLogin {
    type Output = rpc::AccountLoginStatus;
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.call(self).await
    }
    fn apply(self, snapshot: &mut Snapshot, status: Self::Output) -> Vec<Effect> {
        let completed = status.completed;
        let account = Arc::make_mut(&mut snapshot.account);
        account.login_status = Some(Arc::new(status));
        if completed {
            account.login = None;
            let (updated, mut effects) = reduce(
                snapshot,
                Event::Intent(Intent::ListAccounts(ListAccounts {})),
            );
            *snapshot = updated;
            effects.push(Effect::execute(LoadModels {}));
            return effects;
        }
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelAccountLogin {
    #[serde(rename = "loginId")]
    pub id: String,
}
impl rpc::RpcMethod for CancelAccountLogin {
    type Output = Map<String, Value>;
    const METHOD: &'static str = "host/account/login/cancel";
}

impl Operation for CancelAccountLogin {
    fn invalidates(&self, _snapshot: &Snapshot) -> bool {
        true
    }
    type Output = Map<String, Value>;
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.call(self).await
    }
    fn apply(self, snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        let account = Arc::make_mut(&mut snapshot.account);
        account.login = None;
        account.login_status = None;
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListFiles {
    pub path: String,
}
impl rpc::RpcMethod for ListFiles {
    type Output = FileList;
    const METHOD: &'static str = "host/file/list";
}

impl Operation for ListFiles {
    fn invalidates(&self, _snapshot: &Snapshot) -> bool {
        true
    }
    type Output = FileList;
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.call(self).await
    }
    fn apply(self, snapshot: &mut Snapshot, files: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.workspace).directory = Some(Arc::new(files));
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadFile {
    pub path: String,
    pub discard_draft: bool,
}
impl rpc::RpcMethod for ReadFile {
    type Output = FileContent;
    const METHOD: &'static str = "host/file/read";
    fn serialize_params<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut params = serializer.serialize_struct("ReadFile", 1)?;
        params.serialize_field("path", &self.path)?;
        params.end()
    }
}

impl Operation for ReadFile {
    fn prepare(&self, snapshot: &mut Snapshot) -> Result<(), String> {
        if self.discard_draft {
            Arc::make_mut(&mut snapshot.file_drafts).remove(&self.path);
        }
        Ok(())
    }
    fn invalidates(&self, _snapshot: &Snapshot) -> bool {
        true
    }
    type Output = FileContent;
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.call(self).await
    }
    fn apply(self, snapshot: &mut Snapshot, file: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.workspace).file = Some(Arc::new(file));
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveFile {
    pub path: String,
}
impl Operation for SaveFile {
    type Output = (FileDraft, FileContent);
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        let submitted = context
            .snapshot
            .file_drafts
            .get(&self.path)
            .ok_or_else(|| PeerError::InvalidMessage("file has no draft to save".into()))?
            .clone();
        let file = context
            .call(&rpc::WriteFile {
                path: &self.path,
                revision: &submitted.revision,
                text: &submitted.text,
            })
            .await?;
        Ok((submitted, file))
    }
    fn apply(self, snapshot: &mut Snapshot, (submitted, file): Self::Output) -> Vec<Effect> {
        Self::rebase_draft(snapshot, &submitted, &file);
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
    fn stale(self, snapshot: &mut Snapshot, (submitted, file): Self::Output) -> Vec<Effect> {
        Self::rebase_draft(snapshot, &submitted, &file);
        Vec::new()
    }
}
impl SaveFile {
    fn rebase_draft(snapshot: &mut Snapshot, submitted: &FileDraft, file: &FileContent) {
        if let Some(current) = snapshot.file_drafts.get(&file.path) {
            if current == submitted {
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

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewWorkspace {
    pub cwd: String,
}
impl rpc::RpcMethod for ReviewWorkspace {
    type Output = WorkspaceReview;
    const METHOD: &'static str = "host/workspace/review";
}

/// Navigation and notifications already belong to an epoch. Their review read
/// shares it instead of dispatching a second intent that invalidates siblings.
pub(super) fn review_workspace(snapshot: &mut Snapshot) -> Option<Effect> {
    if !snapshot.connected || snapshot.navigation.cwd.is_empty() {
        return None;
    }
    let operation = ReviewWorkspace {
        cwd: snapshot.navigation.cwd.clone(),
    };
    operation
        .prepare(snapshot)
        .expect("selecting a review directory is infallible");
    Some(Effect::execute(operation))
}

impl Operation for ReviewWorkspace {
    fn invalidates(&self, snapshot: &Snapshot) -> bool {
        snapshot.workspace.review_cwd.as_ref() != Some(&self.cwd)
    }
    fn prepare(&self, snapshot: &mut Snapshot) -> Result<(), String> {
        let workspace = Arc::make_mut(&mut snapshot.workspace);
        if workspace.review_cwd.as_ref() != Some(&self.cwd) {
            workspace.review = None;
        }
        workspace.review_cwd = Some(self.cwd.clone());
        Ok(())
    }
    type Output = WorkspaceReview;
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.call(self).await
    }
    fn apply(self, snapshot: &mut Snapshot, review: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.workspace).review = Some(Arc::new(review));
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadWorktreeSettings {}
impl rpc::RpcMethod for ReadWorktreeSettings {
    type Output = super::WorktreeSettings;
    const METHOD: &'static str = "host/worktree/settings/read";
}

impl Operation for ReadWorktreeSettings {
    type Output = super::WorktreeSettings;
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.call(self).await
    }
    fn apply(self, snapshot: &mut Snapshot, settings: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.workspace).settings = Some(Arc::new(settings));
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UpdateWorktreeSettings {
    pub settings: WorktreeSettings,
}
impl rpc::RpcMethod for UpdateWorktreeSettings {
    type Output = super::WorktreeSettings;
    const METHOD: &'static str = "host/worktree/settings/update";
}

impl Operation for UpdateWorktreeSettings {
    type Output = super::WorktreeSettings;
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.call(self).await
    }
    fn apply(self, snapshot: &mut Snapshot, settings: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.workspace).settings = Some(Arc::new(settings));
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListThreads {
    #[serde(default)]
    #[cfg_attr(feature = "bindings", uniffi(default = true))]
    pub title_only: bool,
    #[serde(flatten)]
    pub query: ListQuery,
}
impl ListThreads {
    pub fn new(query: ListQuery) -> Self {
        Self {
            title_only: true,
            query,
        }
    }
}
impl rpc::RpcMethod for ListThreads {
    type Output = ThreadList;
    const METHOD: &'static str = "host/thread/list";
}

impl Operation for ListThreads {
    fn invalidates(&self, snapshot: &Snapshot) -> bool {
        self.query != *snapshot.list_query
    }
    fn prepare(&self, snapshot: &mut Snapshot) -> Result<(), String> {
        snapshot.list_query = Arc::new(self.query.clone());
        Ok(())
    }
    type Output = ThreadList;
    const ORDERED: bool = true;
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.call(self).await
    }
    fn apply(self, snapshot: &mut Snapshot, threads: Self::Output) -> Vec<Effect> {
        snapshot.threads = Some(Arc::new(threads));
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadItem {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
}
impl rpc::RpcMethod for ReadItem {
    type Output = rpc::ItemResponse;
    const METHOD: &'static str = "host/thread/item/read";
    fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
        if output.item.id == self.item_id.as_str() {
            Ok(())
        } else {
            Err("item ID does not match")
        }
    }
}

impl Operation for ReadItem {
    type Output = rpc::ItemResponse;
    const ORDERED: bool = true;
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.call(self).await
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

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadThread {
    pub thread_id: String,
    #[cfg_attr(feature = "bindings", uniffi(default = true))]
    pub include_turns: bool,
    #[cfg_attr(feature = "bindings", uniffi(default = true))]
    pub paginate_history: bool,
    #[cfg_attr(feature = "bindings", uniffi(default = true))]
    pub defer_item_details: bool,
    #[cfg_attr(feature = "bindings", uniffi(default = false))]
    pub open: bool,
}
impl ReadThread {
    pub fn new(thread_id: String) -> Self {
        Self {
            thread_id,
            include_turns: true,
            paginate_history: true,
            defer_item_details: true,
            open: false,
        }
    }
    pub fn open(thread_id: String) -> Self {
        Self {
            open: true,
            ..Self::new(thread_id)
        }
    }
}
impl rpc::RpcMethod for ReadThread {
    type Output = crate::models::ThreadResponse;
    const METHOD: &'static str = "host/thread/read";
    fn serialize_params<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut params = serializer.serialize_struct("ReadThread", 4)?;
        params.serialize_field("threadId", &self.thread_id)?;
        params.serialize_field("includeTurns", &self.include_turns)?;
        params.serialize_field("paginateHistory", &self.paginate_history)?;
        params.serialize_field("deferItemDetails", &self.defer_item_details)?;
        params.end()
    }
    fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
        rpc::validate_thread(output, Some(self.thread_id.as_str()))
    }
}

impl Operation for ReadThread {
    fn invalidates(&self, _snapshot: &Snapshot) -> bool {
        self.open
    }
    type Output = crate::models::ThreadResponse;
    const ORDERED: bool = true;
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.call(self).await
    }
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        if self.open {
            open_thread(snapshot, output.thread, output.model)
        } else {
            refresh_thread(snapshot, output.thread)
        }
    }
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        if self.open {
            refresh_thread(snapshot, output.thread)
        } else {
            Vec::new()
        }
    }
}

fn open_thread(snapshot: &mut Snapshot, thread: Thread, model: Option<String>) -> Vec<Effect> {
    let id = thread.id.clone();
    let cwd = thread.cwd.clone().unwrap_or_default();
    let path = thread.path.clone();
    let external = thread
        .status
        .as_ref()
        .is_some_and(|status| status.kind == "notLoaded");
    let mut effects = refresh_thread(snapshot, thread);
    if let Some(id) = id {
        if snapshot.navigation.cwd != cwd {
            clear_workspace_location(Arc::make_mut(&mut snapshot.workspace));
        }
        let navigation = Arc::make_mut(&mut snapshot.navigation);
        if let Some(watch_id) = navigation.watch_id.take() {
            effects.push(Effect::execute(Unwatch {
                watch_key: 1,
                watch_id,
            }));
        }
        navigation.watch_thread_id = None;
        navigation.thread_id = Some(id.clone());
        if snapshot.activity.unread.contains(&id) {
            Arc::make_mut(&mut snapshot.activity).unread.remove(&id);
        }
        navigation.draft_key = id.clone();
        navigation.cwd = cwd;
        // Loaded threads stream native events. Their advertised rollout may
        // not be materialized yet, so file changes must not trigger hydration.
        if external && path.is_some() {
            navigation.watch_id = Some(snapshot.epoch);
            navigation.watch_thread_id = Some(id.clone());
            effects.push(Effect::execute(Watch {
                thread_id: id.clone(),
                watch_key: 1,
                watch_id: snapshot.epoch,
                path,
            }));
        }
        if let Some(model) = model {
            let (updated, _) = reduce(
                snapshot,
                Event::Intent(Intent::SelectModel {
                    thread_id: id,
                    model,
                }),
            );
            *snapshot = updated;
        }
    }
    effects.extend(review_workspace(snapshot));
    effects
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

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkThread {
    pub thread_id: String,
    pub last_turn_id: String,
    #[cfg_attr(feature = "bindings", uniffi(default = false))]
    pub exclude_turns: bool,
}
impl ForkThread {
    pub fn new(thread_id: String, last_turn_id: String) -> Self {
        Self {
            thread_id,
            last_turn_id,
            exclude_turns: false,
        }
    }
}
impl rpc::RpcMethod for ForkThread {
    type Output = crate::models::ThreadResponse;
    const METHOD: &'static str = "thread/fork";
    fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
        rpc::validate_thread(output, None)
    }
}

impl Operation for ForkThread {
    fn invalidates(&self, _snapshot: &Snapshot) -> bool {
        true
    }
    type Output = crate::models::ThreadResponse;
    const ORDERED: bool = true;
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.call(self).await
    }
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        let mut effects = open_thread(snapshot, output.thread, output.model);
        if snapshot.threads.is_some() {
            effects.push(Effect::execute(ListThreads::new(
                (*snapshot.list_query).clone(),
            )));
        }
        effects
    }
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        refresh_thread(snapshot, output.thread)
    }
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::StartedThread {
            id: output.thread.id.clone().expect("validated thread ID"),
        }
    }
}
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartThread {
    #[serde(skip_serializing_if = "empty_cwd")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}
fn empty_cwd(cwd: &Option<String>) -> bool {
    cwd.as_deref().is_none_or(|cwd| cwd.trim().is_empty())
}
impl rpc::RpcMethod for StartThread {
    type Output = crate::models::ThreadResponse;
    const METHOD: &'static str = "host/thread/start";
    fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
        rpc::validate_thread(output, None)
    }
}

impl Operation for StartThread {
    type Output = crate::models::ThreadResponse;
    const ORDERED: bool = true;
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.call(self).await
    }
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        refresh_thread(snapshot, output.thread)
    }
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        self.apply(snapshot, output)
    }
    fn outcome(output: &mut Self::Output) -> Outcome {
        ForkThread::outcome(output)
    }
}
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dictate {
    pub draft_key: String,
    pub audio: Vec<u8>,
    pub send: bool,
    pub client_user_message_id: String,
}

impl Operation for Dictate {
    type Output = (Arc<Draft>, rpc::Transcription);
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        let draft = context
            .snapshot
            .drafts
            .get(&self.draft_key)
            .cloned()
            .unwrap_or_default();
        Ok((
            draft,
            context
                .call(&rpc::Transcribe {
                    audio: Base64Bytes(&self.audio),
                })
                .await?,
        ))
    }
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        if !self.send {
            return self.stale(snapshot, output);
        }
        let (mut draft, output) = output;
        let Self {
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
    fn stale(self, snapshot: &mut Snapshot, (_, output): Self::Output) -> Vec<Effect> {
        let draft = Arc::make_mut(
            Arc::make_mut(&mut snapshot.drafts)
                .entry(self.draft_key)
                .or_default(),
        );
        append_transcript(&mut draft.text, &output.text);
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResizeTerminal {
    #[serde(rename = "processHandle")]
    pub handle: String,
    pub size: rpc::TerminalSize,
}
impl rpc::RpcMethod for ResizeTerminal {
    type Output = Map<String, Value>;
    const METHOD: &'static str = "process/resizePty";
}

impl Operation for ResizeTerminal {
    fn terminal_handle(&self) -> Option<&str> {
        Some(&self.handle)
    }
    type Output = Map<String, Value>;

    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.call(self).await
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Interrupt {
    pub thread_id: String,
    pub turn_id: String,
}
impl rpc::RpcMethod for Interrupt {
    type Output = Map<String, Value>;
    const METHOD: &'static str = "turn/interrupt";
}

impl Operation for Interrupt {
    type Output = Map<String, Value>;

    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.call(self).await
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Watch {
    pub thread_id: String,
    pub watch_key: u64,
    pub watch_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}
impl rpc::RpcMethod for Watch {
    type Output = Map<String, Value>;
    const METHOD: &'static str = "host/thread/watch";
}

impl Operation for Watch {
    type Output = Map<String, Value>;
    fn prepare(&self, snapshot: &mut Snapshot) -> Result<(), String> {
        let navigation = Arc::make_mut(&mut snapshot.navigation);
        navigation.watch_id = Some(self.watch_id);
        navigation.watch_thread_id = Some(self.thread_id.clone());
        Ok(())
    }
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.call(self).await
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Unwatch {
    pub watch_key: u64,
    pub watch_id: u64,
}
impl rpc::RpcMethod for Unwatch {
    type Output = Map<String, Value>;
    const METHOD: &'static str = "host/thread/unwatch";
}

impl Operation for Unwatch {
    fn disconnected_is_complete(&self) -> bool {
        true
    }
    type Output = Map<String, Value>;
    fn prepare(&self, snapshot: &mut Snapshot) -> Result<(), String> {
        if snapshot.navigation.watch_id == Some(self.watch_id) {
            let navigation = Arc::make_mut(&mut snapshot.navigation);
            navigation.watch_id = None;
            navigation.watch_thread_id = None;
        }
        Ok(())
    }
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.call(self).await
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadOlder {
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub cursor: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "bindings", uniffi(default = true))]
    pub defer_item_details: bool,
}
impl ReadOlder {
    pub fn new(thread_id: String, turn_id: Option<String>, cursor: Option<String>) -> Self {
        Self {
            thread_id,
            turn_id,
            cursor,
            defer_item_details: true,
        }
    }
}
impl rpc::RpcMethod for ReadOlder {
    type Output = crate::models::ThreadResponse;
    const METHOD: &'static str = "host/thread/turns/list";
    fn method(&self) -> &'static str {
        if self.turn_id.is_some() {
            "host/thread/items/list"
        } else {
            Self::METHOD
        }
    }
    fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
        rpc::validate_thread(output, Some(self.thread_id.as_str()))
    }
}

impl Operation for ReadOlder {
    type Output = crate::models::ThreadResponse;
    const ORDERED: bool = true;
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.call(self).await
    }
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        let id = &self.thread_id;
        if let Some(current) = snapshot.conversations.get(id) {
            match older(
                current,
                &output.thread,
                self.turn_id.as_deref(),
                self.cursor.as_deref(),
            ) {
                Ok(merged) => {
                    Arc::make_mut(&mut snapshot.conversations).insert(id.clone(), Arc::new(merged));
                    reconcile_pending(snapshot, id);
                }
                Err(error) => snapshot.error = Some(error),
            }
        }

        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteTerminal {
    pub handle: String,
    pub data: Vec<u8>,
}
struct Base64Bytes<'a>(&'a [u8]);
impl Serialize for Base64Bytes<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&base64::display::Base64Display::new(
            self.0,
            &base64::engine::general_purpose::STANDARD,
        ))
    }
}
impl rpc::RpcMethod for WriteTerminal {
    type Output = Map<String, Value>;
    const METHOD: &'static str = TerminalInput::METHOD;
    fn serialize_params<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        TerminalInput(&self.handle, &self.data).serialize(serializer)
    }
}
struct TerminalInput<'a>(&'a str, &'a [u8]);
impl Serialize for TerminalInput<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut params = serializer.serialize_struct("WriteTerminal", 2)?;
        params.serialize_field("processHandle", self.0)?;
        params.serialize_field("deltaBase64", &Base64Bytes(self.1))?;
        params.end()
    }
}
impl rpc::RpcMethod for TerminalInput<'_> {
    type Output = Map<String, Value>;
    const METHOD: &'static str = "process/writeStdin";
}

impl Operation for WriteTerminal {
    fn terminal_handle(&self) -> Option<&str> {
        Some(&self.handle)
    }
    type Output = ();

    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        // The actor serializes every chunk of one paste; serialization encodes into the JSON buffer.
        for chunk in self.data.chunks(16 * 1024) {
            context.call(&TerminalInput(&self.handle, chunk)).await?;
        }
        Ok(())
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadFile {
    pub source: String,
    pub destination: String,
}
impl Operation for DownloadFile {
    type Output = ();

    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        let session = context.session.ok_or_else(|| {
            PeerError::InvalidMessage("binary transfers require an iroh session".into())
        })?;
        crate::transfers::download_file(
            context.peer,
            || async { session.open_stream().await.map_err(std::io::Error::other) },
            std::path::Path::new(&self.source),
            std::path::Path::new(&self.destination),
        )
        .await
        .map_err(|error| PeerError::InvalidMessage(error.to_string()))
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadSessionImages {
    pub thread_id: String,
}
impl Operation for LoadSessionImages {
    type Output = Vec<rpc::SessionImage>;
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::SessionImages {
            images: std::mem::take(output),
        }
    }
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.client.session_images(&self.thread_id).await
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadHostManagement {}
impl Operation for LoadHostManagement {
    type Output = (HostStatus, Vec<RemoteHost>);

    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        let (status, remotes) = tokio::try_join!(
            context.client.call(&rpc::ReadHostStatus {}),
            context.client.call(&rpc::ListRemoteHosts {})
        )?;
        Ok((status.value, remotes.value))
    }
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        let management = Arc::make_mut(&mut snapshot.management);
        management.status = Some(Arc::new(output.0));
        management.remotes = output.1;
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadModels {}
impl Operation for LoadModels {
    type Output = Vec<Model>;

    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.client.models().await
    }
    fn apply(self, snapshot: &mut Snapshot, models: Self::Output) -> Vec<Effect> {
        let drafts = snapshot.drafts.clone();
        for (id, previous_draft) in drafts.iter() {
            let settings = supported_settings(previous_draft, &models);
            if settings
                != (
                    previous_draft.model.as_deref(),
                    previous_draft.effort.as_deref(),
                    previous_draft.service_tier.as_deref(),
                )
            {
                let draft = Arc::make_mut(Arc::make_mut(&mut snapshot.drafts).get_mut(id).unwrap());
                draft.model = settings.0.map(str::to_owned);
                draft.effort = settings.1.map(str::to_owned);
                draft.service_tier = settings.2.map(str::to_owned);
            }
        }
        snapshot.models = Arc::new(models);
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Respond {
    pub request_id: Value,
    pub answer: Answer,
}
impl Operation for Respond {
    type Output = ();
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        self.apply(snapshot, output)
    }
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        let request = context
            .snapshot
            .requests
            .get(&self.request_id.to_string())
            .ok_or_else(|| {
                PeerError::InvalidMessage("server request is no longer pending".into())
            })?;
        context.client.respond(request, &self.answer).await
    }
    fn apply(self, snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.requests).remove(&self.request_id.to_string());
        Vec::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartSubmission {
    pub draft_key: String,
    pub cwd: Option<String>,
    pub client_user_message_id: String,
    pub draft: Arc<Draft>,
}
impl Operation for StartSubmission {
    type Output = crate::models::ThreadResponse;
    const ORDERED: bool = true;
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        self.complete(snapshot, output.thread, false)
    }
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::StartedThread {
            id: output.thread.id.clone().expect("validated thread ID"),
        }
    }
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context
            .call(&StartThread {
                cwd: self.cwd.clone(),
                model: self.draft.model.clone(),
            })
            .await
    }
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        self.complete(snapshot, output.thread, true)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendSubmission {
    pub thread_id: String,
    pub client_user_message_id: String,
    pub draft: Arc<Draft>,
}
impl Operation for SendSubmission {
    type Output = Option<String>;
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        self.apply(snapshot, output)
    }
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::Submitted {
            turn_id: output.clone(),
        }
    }
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        let target = submission_target(
            context
                .snapshot
                .conversations
                .get(&self.thread_id)
                .map(Arc::as_ref),
            context.snapshot.threads.as_ref().and_then(|list| {
                list.data
                    .iter()
                    .find(|thread| thread.id.as_ref() == Some(&self.thread_id))
            }),
            context
                .snapshot
                .activity
                .active
                .get(&self.thread_id)
                .copied(),
        )?;
        let mut input = Vec::with_capacity(
            self.draft.attachments.len() + usize::from(!self.draft.text.is_empty()),
        );
        if !self.draft.text.is_empty() {
            input.push(Input::Text {
                text: &self.draft.text,
                text_elements: &[],
            });
        }
        for attachment in &self.draft.attachments {
            input.push(if attachment.is_image {
                Input::LocalImage {
                    path: &attachment.path,
                }
            } else {
                Input::Mention {
                    path: &attachment.path,
                    name: &attachment.name,
                }
            });
        }
        let reply = context
            .client
            .submit(
                &Submission {
                    thread_id: &self.thread_id,
                    client_user_message_id: &self.client_user_message_id,
                    input: &input,
                    model: self.draft.model.as_deref(),
                    effort: self.draft.effort.as_deref(),
                    service_tier: self.draft.service_tier.as_deref(),
                },
                target,
            )
            .await?;
        Ok(reply.value)
    }
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        let Self {
            thread_id,
            client_user_message_id,
            draft,
        } = self;
        let turn_id = output;

        let draft = snapshot
            .pending_submissions
            .get(&client_user_message_id)
            .and_then(|pending| pending.clear_draft.as_ref())
            .unwrap_or(&draft);
        if let Some(current) = snapshot.drafts.get(&thread_id) {
            let clear_text = !current.text.is_empty() && current.text == draft.text;
            let sent_attachment = |attachment: &Attachment| {
                draft
                    .attachments
                    .iter()
                    .any(|sent| sent.path == attachment.path)
            };
            if clear_text || current.attachments.iter().any(sent_attachment) {
                let retained = Draft {
                    text: if clear_text {
                        String::new()
                    } else {
                        current.text.clone()
                    },
                    attachments: current
                        .attachments
                        .iter()
                        .filter(|attachment| !sent_attachment(attachment))
                        .cloned()
                        .collect(),
                    model: current.model.clone(),
                    effort: current.effort.clone(),
                    service_tier: current.service_tier.clone(),
                };
                Arc::make_mut(&mut snapshot.drafts).insert(thread_id.clone(), Arc::new(retained));
            }
        }
        if let Some(pending) =
            Arc::make_mut(&mut snapshot.pending_submissions).get_mut(&client_user_message_id)
        {
            let pending = Arc::make_mut(pending);
            pending.accepted = true;
            if pending.turn_id.is_none() {
                pending.turn_id = turn_id;
            }
        }
        reconcile_pending(snapshot, &thread_id);

        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadAttachment {
    pub draft_key: String,
    pub attachment: Attachment,
    pub directory: String,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairRemoteHost {
    pub invitation: Invitation,
    pub name: String,
}

fn refresh_thread(snapshot: &mut Snapshot, incoming: Thread) -> Vec<Effect> {
    let Some(id) = incoming.id.clone().filter(|id| !id.trim().is_empty()) else {
        snapshot.error = Some("thread ID is missing".into());
        return Vec::new();
    };
    let thread = match snapshot.conversations.get(&id) {
        Some(current) => refresh(current, &incoming),
        None => incoming,
    };
    Arc::make_mut(&mut snapshot.conversations).insert(id.clone(), Arc::new(thread));
    reconcile_pending(snapshot, &id);

    Vec::new()
}

impl StartSubmission {
    fn complete(self, snapshot: &mut Snapshot, thread: Thread, current_view: bool) -> Vec<Effect> {
        let Self {
            draft_key,
            client_user_message_id,
            draft,
            ..
        } = self;
        let Some(id) = thread.id.clone() else {
            snapshot.error = Some("thread ID is missing".into());
            return Vec::new();
        };
        let current = snapshot.drafts.get(&draft_key);
        let original = snapshot
            .pending_submissions
            .get(&client_user_message_id)
            .and_then(|pending| pending.clear_draft.as_ref())
            .unwrap_or(&draft);
        let remove_original = current_view || current == Some(original);
        let target = if current_view {
            current.unwrap_or(original).clone()
        } else {
            original.clone()
        };
        let mut effects = if current_view {
            open_thread(snapshot, thread, None)
        } else {
            refresh_thread(snapshot, thread)
        };
        let drafts = Arc::make_mut(&mut snapshot.drafts);
        if remove_original {
            drafts.remove(&draft_key);
        }
        drafts.insert(id.clone(), target);
        if let Some(pending) =
            Arc::make_mut(&mut snapshot.pending_submissions).get_mut(&client_user_message_id)
        {
            Arc::make_mut(pending).draft_key = id.clone();
        }
        effects.push(Effect::Submit(SendSubmission {
            thread_id: id,
            client_user_message_id,
            draft,
        }));
        if snapshot.threads.is_some() {
            effects.push(Effect::execute(ListThreads::new(
                (*snapshot.list_query).clone(),
            )));
        }
        effects
    }
}
