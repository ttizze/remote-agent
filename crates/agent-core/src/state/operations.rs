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
    StartAccountLogin(StartAccountLogin),
    ReadAccountLogin(ReadAccountLogin),
    CancelAccountLogin(CancelAccountLogin),
    ForkThread(ForkThread),
    StartTerminal(StartTerminal),
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
    ExpandThreadList {
        project_id: Option<String>,
        projects: bool,
    },
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
/// Only operations with durable side effects apply stale results or override `stale`.
pub trait Operation: Send + Sync + std::fmt::Debug + Sized + 'static {
    type Output: Send + std::fmt::Debug + 'static;
    const ORDERED: bool = false;
    const INVALIDATES: bool = false;
    const APPLY_WHEN_STALE: bool = false;
    /// Identifies a submission step whose completion and failure belong to the same dispatch.
    fn submission_id(&self) -> Option<&str> {
        None
    }
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

struct Base64Bytes<'a>(&'a [u8]);
impl Serialize for Base64Bytes<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&base64::display::Base64Display::new(
            self.0,
            &base64::engine::general_purpose::STANDARD,
        ))
    }
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
