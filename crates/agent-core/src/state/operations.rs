use super::*;
use crate::{
    peer::PeerError,
    store::{Execution, Outcome},
};
use agent_protocol::operations as rpc;
use rpc::{Input, Submission};

macro_rules! rpc_operation {
    ($parent:ident.$field:ident) => {
        rpc_operation!();
        fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
            Arc::make_mut(&mut snapshot.$parent).$field = Some(Arc::new(output));
            Vec::new()
        }
    };
    () => {
        type Output = <Self as rpc::RpcMethod>::Output;
        async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
            context.call(self).await
        }
    };
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum Intent {
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
    AddProject(AddProject),
    ExpandThreadList {
        project_id: Option<String>,
        projects: bool,
    },
    CreateSession(CreateSession),
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

/// Typed state application after the Store has checked its single epoch.
pub trait Operation: Send + Sync + std::fmt::Debug + Sized + 'static {
    type Output: Send + std::fmt::Debug + 'static;
    const INVALIDATES: bool = false;
    /// Metadata refreshes publish later without holding the initiating receipt.
    const BACKGROUND: bool = false;
    const STALE_POLICY: StalePolicy = StalePolicy::Discard;
    /// Identifies a submission step whose completion and failure belong to the same dispatch.
    fn submission_id(&self) -> Option<&str> {
        None
    }
    fn item_read(&self) -> Option<&ReadItem> {
        None
    }
    fn terminal_handle(&self) -> Option<&str> {
        None
    }
    fn run(
        &self,
        context: &mut Execution<'_>,
    ) -> impl Future<Output = Result<Self::Output, PeerError>> + Send;
    fn invalidates(&self, _snapshot: &Snapshot) -> bool {
        Self::INVALIDATES
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
