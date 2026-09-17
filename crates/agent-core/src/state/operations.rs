use super::*;
use crate::{
    client as rpc,
    peer::PeerError,
    store::{Execution, Outcome},
};
use rpc::{Input, Submission, submission_target};

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
    ListAccounts(ListAccounts),
    SelectAccount(SelectAccount),
    LogoutAccount(LogoutAccount),
    StartAccountLogin(StartAccountLogin),
    SubmitAccountLogin(SubmitAccountLogin),
    ReadAccountLogin(ReadAccountLogin),
    CancelAccountLogin(CancelAccountLogin),
    ForkThread(ForkThread),
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
    ListThreads(ListThreads),
    AddProject(AddProject),
    ExpandThreadList {
        project_id: Option<String>,
        projects: bool,
    },
    StartThread(StartThread),
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
    LoadVisualization(LoadVisualization),
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
        thread_id: String,
    },
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
/// Only operations with durable side effects apply stale results or override `stale`.
pub trait Operation: Send + Sync + std::fmt::Debug + Sized + 'static {
    type Output: Send + std::fmt::Debug + 'static;
    const INVALIDATES: bool = false;
    const APPLY_WHEN_STALE: bool = false;
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
        if Self::APPLY_WHEN_STALE {
            self.apply(snapshot, output)
        } else {
            Vec::new()
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

pub(super) fn add_attachment(next: &mut Snapshot, draft_key: String, attachment: Attachment) {
    Arc::make_mut(
        Arc::make_mut(&mut next.drafts)
            .entry(draft_key)
            .or_default(),
    )
    .attachments
    .push(attachment);
}
