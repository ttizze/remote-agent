use super::*;
use crate::{
    peer::PeerError,
    store::{Execution, Outcome},
};
use agent_protocol::operations as rpc;
use rpc::{Input, Submission};

macro_rules! no_input {
    () => {
        type Input = ();
        fn capture(&self, _: &Snapshot) -> Result<(), PeerError> {
            Ok(())
        }
    };
}

macro_rules! rpc_operation {
    ($parent:ident.$field:ident) => {
        rpc_operation!();
        fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
            Arc::make_mut(&mut snapshot.$parent).$field = Some(Arc::new(output));
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
    ReadTaskActivity(ReadTaskActivity),
    RegisterLiveActivity(RegisterLiveActivity),
    UnregisterLiveActivity(UnregisterLiveActivity),
    ReadPermissionSettings(ReadPermissionSettings),
    UpdatePermissionSettings(UpdatePermissionSettings),
    ListAccounts(ListAccounts),
    SelectAccount(SelectAccount),
    SelectAccountForDraft(SelectAccountForDraft),
    LogoutAccount(LogoutAccount),
    StartAccountLogin(StartAccountLogin),
    SubmitAccountLogin(SubmitAccountLogin),
    ReadAccountLogin(ReadAccountLogin),
    CancelAccountLogin(CancelAccountLogin),
    ForkSession(ForkSession),
    StartTerminal(StartTerminal),
    DetachTerminal(DetachTerminal),
    KillTerminal(KillTerminal),
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
    ListWorktrees(ListWorktrees),
    RemoveWorktree(RemoveWorktree),
    ListSessions(ListSessions),
    ListProjectSessions(ListProjectSessions),
    ListAgents(ListAgents),
    WatchAgents {
        thread_id: Option<crate::session::SessionRef>,
    },
    AddProject(AddProject),
    ExpandThreadList {
        project_id: Option<String>,
    },
    SetProjectExpanded {
        project_id: String,
        expanded: bool,
    },
    RefreshProject {
        project_id: String,
    },
    CreateSession(CreateSession),
    ReadThread(ReadThread),
    OpenRequest(OpenRequest),
    ReadItem(ReadItem),
    LoadTurnItems(LoadTurnItems),
    ResizeTerminal(ResizeTerminal),
    Interrupt(Interrupt),
    ShowThreadList,
    WriteTerminal(WriteTerminal),
    AcknowledgeTerminal {
        handle: String,
        sequence: u64,
    },
    AddAttachment {
        draft_key: DraftKey,
        attachment: Attachment,
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
    LoadModels(LoadModels),
    SelectNewChatModel {
        scope: super::ModelDefaultsScope,
        model: Option<crate::models::ModelRef>,
    },
    SelectDefaultModel {
        scope: super::ModelDefaultsScope,
        provider: crate::session::ProviderKind,
        model: Option<crate::models::ModelRef>,
    },
    SelectDefaultEffort {
        scope: super::ModelDefaultsScope,
        provider: crate::session::ProviderKind,
        effort: Option<String>,
    },
    SelectDefaultServiceTier {
        scope: super::ModelDefaultsScope,
        provider: crate::session::ProviderKind,
        service_tier: Option<String>,
    },
    InheritModelDefaults {
        scope: super::ModelDefaultsScope,
    },
    SetDraft {
        thread_id: DraftKey,
        draft: Draft,
    },
    EditComposer {
        thread_id: DraftKey,
        text: String,
        cursor: u32,
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
    Submit {
        thread_id: Option<crate::session::SessionRef>,
        client_user_message_id: agent_protocol::ids::ClientInputId,
    },
    RestoreUnknownSubmission {
        client_user_message_id: agent_protocol::ids::ClientInputId,
    },
    DiscardUnknownSubmission {
        client_user_message_id: agent_protocol::ids::ClientInputId,
    },
    Respond(Respond),
}

pub enum StalePolicy {
    Discard,
    Apply,
    /// Fetch current data again instead of publishing a response from an older epoch.
    Retry,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum OperationKey {
    TaskActivity,
    LiveActivity {
        activity_id: String,
    },
    TurnItems {
        session: crate::session::SessionRef,
        turn: agent_protocol::ids::TurnId,
    },
    SessionList,
    ProjectList {
        project_id: String,
    },
    Agents {
        session: crate::session::SessionRef,
    },
    History {
        session: crate::session::SessionRef,
    },
    Item {
        item: ReadItem,
    },
    Directory,
    File,
    SaveFile {
        path: String,
    },
    WorkspaceReview,
    Models,
    Accounts,
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
    Request {
        id: agent_protocol::ids::RequestId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationPhase {
    Running,
    Failed { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationState {
    pub generation: u64,
    pub phase: OperationPhase,
}

#[derive(Debug)]
pub enum Scheduling {
    Concurrent,
    Control,
    LatestTaskActivity,
    LatestList(crate::models::ListQuery),
    LatestAgents(crate::session::SessionRef),
    LatestProject(String),
    LatestReview,
    Item(ReadItem),
    Terminal { handle: String, starts: bool },
}
impl Scheduling {
    pub(crate) fn latest_key(&self) -> Option<OperationKey> {
        match self {
            Self::LatestTaskActivity => Some(OperationKey::TaskActivity),
            Self::LatestList(_) => Some(OperationKey::SessionList),
            Self::LatestProject(project_id) => Some(OperationKey::ProjectList {
                project_id: project_id.clone(),
            }),
            Self::LatestAgents(session) => Some(OperationKey::Agents {
                session: session.clone(),
            }),
            Self::LatestReview => Some(OperationKey::WorkspaceReview),
            _ => None,
        }
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
    type Input: Send + std::fmt::Debug + 'static;
    fn capture(&self, snapshot: &Snapshot) -> Result<Self::Input, PeerError>;
    fn key(&self) -> Option<OperationKey> {
        None
    }
    type Output: Send + std::fmt::Debug + 'static;
    /// Metadata refreshes publish later without holding the initiating receipt.
    const BACKGROUND: bool = false;
    const STALE_POLICY: StalePolicy = StalePolicy::Discard;
    /// Identifies a submission step whose completion and failure belong to the same dispatch.
    fn submission_id(&self) -> Option<&str> {
        None
    }
    fn scheduling(&self) -> Scheduling {
        Scheduling::Concurrent
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
    fn prepare(&mut self, _snapshot: &mut Snapshot) -> Result<(), String> {
        Ok(())
    }
    fn apply(self, _snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        Vec::new()
    }
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        match Self::STALE_POLICY {
            StalePolicy::Apply => self.apply(snapshot, output),
            StalePolicy::Retry if snapshot.connected => vec![Effect::execute(self)],
            StalePolicy::Discard | StalePolicy::Retry => Vec::new(),
        }
    }
    fn complete(
        self,
        snapshot: &mut Snapshot,
        output: Self::Output,
        current: bool,
    ) -> Result<Vec<Effect>, PeerError> {
        Ok(if current {
            self.apply(snapshot, output)
        } else {
            self.stale(snapshot, output)
        })
    }
    fn outcome(_output: &mut Self::Output) -> Outcome {
        Outcome::Applied
    }
}

mod permissions;
pub use permissions::*;
mod composer;
pub use composer::*;
mod accounts;
pub use accounts::*;
mod terminal;
pub use terminal::*;
mod hosts;
pub use hosts::*;
mod workspace;
pub use workspace::*;
mod threads;
pub use threads::*;
mod submission;
pub use submission::*;

pub(super) fn add_attachment(next: &mut Snapshot, draft_key: DraftKey, attachment: Attachment) {
    Arc::make_mut(
        Arc::make_mut(&mut next.drafts)
            .entry(draft_key)
            .or_default(),
    )
    .attachments
    .push(attachment);
}
