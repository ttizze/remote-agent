use super::*;
use crate::{
    peer::PeerError,
    store::{Execution, Outcome},
};
use agent_protocol::operations as rpc;
use rpc::Input;

macro_rules! no_input {
    () => {
        type Input = ();
        fn capture(&self, _: &Snapshot, _: &str) -> Result<(), PeerError> {
            Ok(())
        }
    };
}

macro_rules! rpc_operation {
    ($group:ident.$parent:ident.$field:ident) => {
        rpc_operation!();
        fn apply(
            self,
            snapshot: &mut Snapshot,
            _pane_id: &str,
            output: Self::Output,
        ) -> Vec<Effect> {
            Arc::make_mut(&mut snapshot.$group.$parent).$field = Some(Arc::new(output));
            Vec::new()
        }
    };
    () => {
        no_input!();
        type Output = <Self as rpc::RpcMethod>::Output;
        async fn run(
            &self,
            _: Self::Input,
            context: &mut Execution<'_>,
        ) -> Result<Self::Output, PeerError> {
            context.call(self).await
        }
    };
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum Intent {
    Stash {
        storage_scope: String,
        thread_id: DraftKey,
        action: crate::composer::stash::StashAction,
    },
    ChangeContext {
        storage_scope: String,
        thread_id: DraftKey,
        action: crate::composer::context::ContextAction,
    },
    UseArtifactTemplate {
        storage_scope: String,
        thread_id: DraftKey,
        template: crate::presentation::markdown::ArtifactTemplate,
    },
    WatchCommandOutput {
        thread_id: crate::session::SessionRef,
        item_id: agent_protocol::ids::ItemId,
        watching: bool,
    },
    EnsureFileMetadata {
        paths: Vec<String>,
        now_ms: u64,
    },
    FileTree {
        key: crate::presentation::files::FileTreeKey,
        action: crate::presentation::files::FileTreeAction,
    },
    DismissError {
        id: ErrorId,
    },
    RecoverError {
        id: ErrorId,
    },
    ManageCodexCli(ManageCodexCli),
    ReadProviderSettings(ReadProviderSettings),
    ReadHostSoftware(ReadHostSoftware),
    SetProviderEnabled(SetProviderEnabled),
    SetConnectionEnabled {
        enabled: bool,
    },
    UploadFeedback(UploadFeedback),
    SetFreshContext {
        storage_scope: String,
        session: crate::session::SessionRef,
        enabled: bool,
    },
    DismissFeedback {
        storage_scope: String,
        session: crate::session::SessionRef,
        id: String,
    },
    RegisterLiveActivity(RegisterLiveActivity),
    UnregisterLiveActivity(UnregisterLiveActivity),
    ReadPermissionSettings(ReadPermissionSettings),
    UpdatePermissionSettings(UpdatePermissionSettings),
    ListAccounts(ListAccounts),
    ReadDshCredentials(ReadDshCredentials),
    UpdateDshCredential(UpdateDshCredential),
    SelectAccount(SelectAccount),
    SelectAccountForDraft(SelectAccountForDraft),
    LogoutAccount(LogoutAccount),
    StartAccountLogin(StartAccountLogin),
    SubmitAccountLogin(SubmitAccountLogin),
    ReadAccountLogin(ReadAccountLogin),
    CancelAccountLogin(CancelAccountLogin),
    ForkSession(ForkSession),
    RewindSession(RewindSession),
    StartTerminal(StartTerminal),
    StartAccountTerminal(StartAccountTerminal),
    DetachTerminal(DetachTerminal),
    KillTerminal(KillTerminal),
    CloseTerminal {
        handle: String,
    },
    Transcribe(Dictate),
    CreateInvitation(CreateInvitation),
    RemoveRemoteHost(RemoveRemoteHost),
    RevokeDevice(RevokeDevice),
    ListFiles(ListFiles),
    ResolveFile(ResolveFile),
    ResolveFileReference(ResolveFileReference),
    ReadFileOpenCapabilities(ReadFileOpenCapabilities),
    OpenInEditor(OpenInEditor),
    SetPreferredEditor {
        editor: agent_protocol::editors::EditorId,
    },
    ReadFile(ReadFile),
    SaveFile(SaveFile),
    WriteWorkspaceFile(WriteWorkspaceFile),
    ReviewWorkspace(ReviewWorkspace),
    ChangeWorkspace(ChangeWorkspace),
    SetWorkspaceComparison {
        cwd: String,
        base_ref: Option<String>,
    },
    ReadWorktreeSettings(ReadWorktreeSettings),
    UpdateWorktreeSettings(UpdateWorktreeSettings),
    ListWorktrees(ListWorktrees),
    RemoveWorktree(RemoveWorktree),
    ListSessions(crate::models::ListQuery),
    ListAgents(ListAgents),
    WatchAgents {
        thread_id: Option<crate::session::SessionRef>,
    },
    AddProject(AddProject),
    ScanProjects(ScanProjects),
    ImportProjects(ImportProjects),
    CloneProject(CloneProject),
    ReadProjectClones(ReadProjectClones),
    ChangeProjectClone(ChangeProjectClone),
    SelectImportProject {
        path: String,
        selected: bool,
    },
    SelectAllImportProjects {
        selected: bool,
    },
    ShowRecentChats,
    ExpandRecentChats,
    ExpandProjects,
    ExpandThreadList {
        project_id: Option<String>,
    },
    SetProjectPinned {
        project_id: String,
        pinned: bool,
    },
    ReorderProject {
        project_id: String,
        target_id: String,
        after: bool,
    },
    MoveChatToProject {
        thread_id: crate::session::SessionRef,
        project_id: String,
    },
    EditProject {
        project_id: String,
        name: String,
        section: String,
    },
    SetProjectSection {
        project_id: String,
        section: String,
    },
    SetProjectHidden {
        project_id: String,
        hidden: bool,
    },
    ArchiveProjectChats {
        project_id: String,
        before: i64,
    },
    RestoreProjectChats {
        project_id: String,
    },
    SetProjectExpanded {
        project_id: String,
        expanded: bool,
    },
    RefreshProject {
        project_id: String,
    },
    CreateSession(CreateSession),
    RetryPreparation(RetryPreparation),
    ReadThread(ReadThread),
    OpenRequest(OpenRequest),
    ReadItem(ReadItem),
    ResizeTerminal(ResizeTerminal),
    Interrupt(Interrupt),
    ShowThreadList,
    WriteTerminal(WriteTerminal),
    AcknowledgeTerminal {
        handle: String,
        sequence: u64,
    },
    RemoveAttachment {
        draft_key: DraftKey,
        index: u32,
    },
    UploadAttachment(UploadAttachment),
    DownloadFile(DownloadFile),
    LoadSessionImages(LoadSessionImages),
    LoadVisualization(LoadVisualization),
    LoadHostName(LoadHostName),
    LoadHostManagement(LoadHostManagement),
    PairRemoteHost(PairRemoteHost),
    NewChat {
        cwd: String,
    },
    SetFileDraft {
        path: String,
        text: String,
    },
    ReadOlder {
        thread_id: crate::session::SessionRef,
    },
    RefreshModels,
    LoadModels(LoadModels),
    ReadModelSettings(ReadModelSettings),
    SetDraft {
        thread_id: DraftKey,
        draft: Draft,
    },
    SelectWorkspace {
        thread_id: DraftKey,
        workspace: Option<agent_protocol::workspace::WorkspaceStrategy>,
    },
    EditComposer {
        thread_id: DraftKey,
        text: String,
        cursor: u32,
    },
    AppendTranscript {
        thread_id: DraftKey,
        text: String,
    },
    InsertInvocation {
        thread_id: DraftKey,
        text: String,
        invocation: agent_protocol::composer::Invocation,
    },
    SetDraftText {
        thread_id: DraftKey,
        text: String,
    },
    QuoteSelection {
        storage_scope: String,
        thread_id: DraftKey,
        text: String,
    },
    SelectModel {
        thread_id: DraftKey,
        model: crate::models::ModelRef,
    },
    SelectEffort {
        thread_id: DraftKey,
        effort: String,
    },
    SelectServiceTier {
        thread_id: DraftKey,
        service_tier: String,
    },
    /// `alternate` does the opposite of the follow-up behavior for this message
    /// (T3 Code's Mod+Enter or Ctrl/⌘-click while a turn runs). While a queued
    /// message is edited, the composer's text replaces it instead.
    Submit {
        thread_id: Option<crate::session::SessionRef>,
        client_user_message_id: agent_protocol::ids::ClientInputId,
        alternate: bool,
    },
    SetComposerPreferences {
        preferences: crate::state::ComposerPreferences,
    },
    Queue {
        thread_id: crate::session::SessionRef,
        action: crate::state::follow_up::QueueAction,
    },
    RestoreUnknownSubmission {
        client_user_message_id: agent_protocol::ids::ClientInputId,
    },
    DiscardUnknownSubmission {
        client_user_message_id: agent_protocol::ids::ClientInputId,
    },
    Respond(Respond),
    AnswerQuestion {
        request_id: agent_protocol::ids::RequestId,
        action: crate::questions::QuestionAction,
    },
}

pub enum StalePolicy {
    Discard,
    Apply,
}

impl Intent {
    pub(super) fn editing_draft(&mut self) -> Option<&mut DraftKey> {
        match self {
            Self::SetDraft { thread_id, .. }
            | Self::Stash { thread_id, .. }
            | Self::ChangeContext { thread_id, .. }
            | Self::UseArtifactTemplate { thread_id, .. }
            | Self::SelectWorkspace { thread_id, .. }
            | Self::EditComposer { thread_id, .. }
            | Self::AppendTranscript { thread_id, .. }
            | Self::InsertInvocation { thread_id, .. }
            | Self::SetDraftText { thread_id, .. }
            | Self::QuoteSelection { thread_id, .. }
            | Self::SelectModel { thread_id, .. }
            | Self::SelectEffort { thread_id, .. }
            | Self::SelectServiceTier { thread_id, .. } => Some(thread_id),
            Self::UploadAttachment(upload) => Some(&mut upload.draft_key),
            Self::Transcribe(dictate) => Some(&mut dictate.draft_key),
            Self::SelectAccountForDraft(account) => Some(&mut account.thread_id),
            Self::StartAccountLogin(login) => login.thread_id.as_mut(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum OperationKey {
    Feedback {
        session: crate::session::SessionRef,
    },
    ProjectClone {
        id: Option<String>,
    },
    InputRecovery {
        input: agent_protocol::ids::ClientInputId,
    },
    CommandOutput {
        session: crate::session::SessionRef,
        item: agent_protocol::ids::ItemId,
    },
    ProjectScan,
    ProjectImport,
    LiveActivity {
        activity_id: Option<String>,
    },
    ListDecorations {
        scope: agent_protocol::operations::ListDecorationScope,
    },
    SessionList,
    ListSearch,
    Agents {
        session: crate::session::SessionRef,
    },
    History {
        session: crate::session::SessionRef,
    },
    Fork {
        session: crate::session::SessionRef,
        turn: agent_protocol::ids::TurnId,
    },
    Rewind {
        session: crate::session::SessionRef,
    },
    Item {
        item: ReadItem,
    },
    Directory {
        pane: String,
    },
    FileOpenCapabilities,
    File {
        pane: String,
    },
    SaveFile {
        path: String,
    },
    WorkspaceReview {
        pane: String,
    },
    WorkspaceAction {
        pane: String,
    },
    CodexCli,
    ProviderSettings,
    HostSoftware,
    Models {
        provider: crate::session::ProviderKind,
    },
    ModelSettings {
        model: crate::models::ModelRef,
    },
    Accounts,
    DshCredentials,
    AccountLogin {
        provider: crate::session::ProviderKind,
    },
    Permissions {
        provider: crate::session::ProviderKind,
    },
    WorktreeSettings,
    Worktrees,
    Submission {
        draft_key: DraftKey,
    },
    Dictation {
        draft_key: DraftKey,
    },
    Attachment {
        draft_key: DraftKey,
    },
    Interrupt {
        session: crate::session::SessionRef,
        turn: agent_protocol::ids::TurnId,
    },
    /// A change to a conversation's queued messages.
    Queue {
        session: crate::session::SessionRef,
    },
    Request {
        id: agent_protocol::ids::RequestId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ErrorId {
    Application { message: String },
    Operation { key: OperationKey, generation: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum OperationPhase {
    Running,
    Failed { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationState {
    pub generation: u64,
    pub phase: OperationPhase,
}

impl OperationKey {
    pub(crate) fn in_pane(mut self, id: &str) -> Self {
        match &mut self {
            Self::Directory { pane }
            | Self::File { pane }
            | Self::WorkspaceReview { pane }
            | Self::WorkspaceAction { pane } => {
                *pane = id.into();
            }
            _ => (),
        }
        self
    }

    pub(crate) fn pane(&self) -> Option<&str> {
        match self {
            Self::Directory { pane }
            | Self::File { pane }
            | Self::WorkspaceReview { pane }
            | Self::WorkspaceAction { pane } => Some(pane),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Scheduling {
    Concurrent,
    Control,
    /// Host-owned state for the whole connection: navigation never makes it
    /// stale, and only the newest read of the key runs.
    LatestHostState(OperationKey),
    LatestAgents(crate::session::SessionRef),
    LatestReview,
    Item(ReadItem),
    Terminal {
        handle: String,
        starts: bool,
    },
}
impl Scheduling {
    /// The coalescing key for the latest read issued from `pane`.
    pub(crate) fn latest_key(&self, pane: &str) -> Option<OperationKey> {
        let key = match self {
            Self::LatestHostState(key) => Some(key.clone()),
            Self::LatestAgents(session) => Some(OperationKey::Agents {
                session: session.clone(),
            }),
            Self::LatestReview => Some(OperationKey::WorkspaceReview {
                pane: String::new(),
            }),
            _ => None,
        };
        key.map(|key| key.in_pane(pane))
    }
    pub(crate) fn item(&self) -> Option<&ReadItem> {
        if let Self::Item(item) = self {
            Some(item)
        } else {
            None
        }
    }
    pub(crate) fn terminal(&self) -> Option<&str> {
        if let Self::Terminal { handle, .. } = self {
            Some(handle)
        } else {
            None
        }
    }
}

/// Typed state application after Store checks the navigation epoch or list query.
pub trait Operation: Send + Sync + std::fmt::Debug + Sized + 'static {
    fn subscription_scope(
        &self,
    ) -> Option<agent_protocol::sync::Scope<crate::session::SessionRef>> {
        None
    }
    type Input: Send + std::fmt::Debug + 'static;
    fn capture(&self, snapshot: &Snapshot, pane_id: &str) -> Result<Self::Input, PeerError>;
    fn key(&self) -> Option<OperationKey> {
        None
    }
    type Output: Send + std::fmt::Debug + 'static;
    /// Metadata refreshes publish later without holding the initiating receipt.
    const BACKGROUND: bool = false;
    /// Untracked failures become application notices unless the caller owns
    /// their presentation, such as an individual file preview or save control.
    const REPORT_ERROR: bool = true;
    const STALE_POLICY: StalePolicy = StalePolicy::Discard;
    /// Identifies a submission step whose completion and failure belong to the same dispatch.
    fn submission_id(&self) -> Option<&str> {
        None
    }
    fn scheduling(&self) -> Scheduling {
        Scheduling::Concurrent
    }
    /// Host and session caches outlive the pane that requested them, as does
    /// an account login. Only results that select or replace a pane's content
    /// follow its navigation.
    fn navigation_bound(&self) -> bool {
        !matches!(self.scheduling(), Scheduling::LatestHostState(_))
            && !matches!(self.key(), Some(OperationKey::AccountLogin { .. }))
    }
    fn run(
        &self,
        input: Self::Input,
        context: &mut Execution<'_>,
    ) -> impl Future<Output = Result<Self::Output, PeerError>> + Send;
    /// Finish prepared state when the scheduler cannot admit this operation.
    fn rejected_output(&self, _input: Self::Input) -> Option<Self::Output> {
        None
    }
    fn invalidates(&self) -> bool {
        false
    }
    fn prepare(&mut self, _snapshot: &mut Snapshot, _pane_id: &str) -> Result<(), String> {
        Ok(())
    }
    fn apply(self, _snapshot: &mut Snapshot, _pane_id: &str, _output: Self::Output) -> Vec<Effect> {
        Vec::new()
    }
    fn stale(self, snapshot: &mut Snapshot, pane_id: &str, output: Self::Output) -> Vec<Effect> {
        match Self::STALE_POLICY {
            StalePolicy::Apply => self.apply(snapshot, pane_id, output),
            StalePolicy::Discard => Vec::new(),
        }
    }
    fn complete(
        self,
        snapshot: &mut Snapshot,
        pane_id: &str,
        output: Self::Output,
        current: bool,
    ) -> Result<Vec<Effect>, PeerError> {
        Ok(if current {
            self.apply(snapshot, pane_id, output)
        } else {
            self.stale(snapshot, pane_id, output)
        })
    }
    fn outcome(_output: &mut Self::Output) -> Outcome {
        Outcome::Applied
    }
}

pub type OperationStates = Arc<BTreeMap<OperationKey, OperationState>>;

mod permissions;
pub use permissions::*;
mod editors;
pub use editors::*;
mod composer;
pub use composer::*;
mod codex_cli;
pub use codex_cli::*;
mod providers;
pub use providers::*;
mod accounts;
pub use accounts::*;
mod terminal;
pub use terminal::*;
mod hosts;
pub use hosts::*;
mod workspace;
pub use workspace::*;
mod file_tree;
pub(crate) use file_tree::file_tree_action;
pub(super) use file_tree::{reconnect_file_trees, refresh_file_trees};
mod file_metadata;
pub(crate) use file_metadata::ensure_file_metadata;
mod file_reference;
pub use file_reference::ResolveFileReference;
mod lists;
pub use lists::*;
mod models;
pub use models::*;
mod sessions;
pub use sessions::*;
mod project_import;
pub use project_import::*;
mod project_clone;
pub use project_clone::*;
mod feedback;
pub use feedback::*;
mod submission;
pub use submission::*;

pub(super) fn page_rows(
    mut rows: Vec<crate::models::Thread>,
    limit: u32,
) -> (Vec<crate::models::Thread>, bool) {
    let limit = limit as usize;
    let more = rows.len() > limit;
    rows.sort_by(|a, b| {
        b.updated_at
            .unwrap_or_default()
            .total_cmp(&a.updated_at.unwrap_or_default())
    });
    rows.truncate(limit);
    (rows, more)
}

#[cfg(test)]
mod page_tests;
