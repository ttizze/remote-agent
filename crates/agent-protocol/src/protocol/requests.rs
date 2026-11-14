//! One contract per native request; provider JSON conversion stays at its boundary.
use super::*;
use crate::{models as m, operations as op, session as s};
macro_rules! contracts {
    ($($variant:ident, $method:literal => ($params:ty, $result:ty) $([$clone:ident $(, $validate:path)?])?),* $(,)?) => {
        // Bind metadata to the operation, not the parameter type: ReadFile and
        // Download deliberately share parameters but have different results.
        pub mod contracts {
            use super::*;
            pub trait Contract {
                type Params;
                type Output;
                fn call(params: Self::Params) -> Call;
                const METHOD: &'static str;
            }
            $(pub struct $variant;
            impl Contract for $variant {
                type Params = $params;
                type Output = $result;
                fn call(params: Self::Params) -> Call { Call::$variant(params) }
                const METHOD: &'static str = $method;
            })*
        }
        $($(contracts!(@$clone $params, $variant $(, $validate)?);)?)*
        #[derive(Debug, Clone, Serialize, Deserialize)]
        #[serde(tag = "method")]
        pub enum Call {
            $(#[serde(rename = $method)] $variant($params)),*,
            #[serde(other)]
            Unsupported,
        }
        impl Call {
            pub fn method(&self) -> &str {
                match self { $(Self::$variant(_) => $method),*, Self::Unsupported => "unsupported" }
            }
            #[cfg(any(test, feature = "test-support"))]
            pub fn params_json(&self) -> Result<Value, serde_json::Error> {
                match self {
                    $(Self::$variant(params) => serde_json::to_value(params)),*,
                    Self::Unsupported => Ok(Value::Null),
                }
            }
        }
        #[cfg(any(test, feature = "test-support"))]
        pub fn from_json(method: &str, params: Value) -> io::Result<Call> {
            Ok(match method {
                $($method => Call::$variant(serde_json::from_value(params).map_err(io::Error::other)?)),*,
                _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("unregistered method: {method}"))),
            })
        }
        #[cfg(any(test, feature = "test-support"))]
        pub fn provider_response(method: &str, line: &str) -> Result<Response, crate::message::RpcMessageError> {
            match method {
                $($method => super::json_boundary::typed_response::<$result>(line)),*,
                _ => Err(crate::message::RpcMessageError::InvalidCombination {reason:"unregistered response"}),
            }
        }
        #[cfg(any(test, feature = "test-support"))]
        pub fn fixture_reply(method: &str, bytes: &[u8]) -> io::Result<Value> {
            match method {
                $($method => Ok(decode::<Response<$result>>(bytes)?.into_value())),*,
                _ => Err(io::Error::new(io::ErrorKind::InvalidInput, format!("unregistered method: {method}"))),
            }
        }
    };
    (@clone $params:ty, $variant:ident $(, $validate:path)?) => {
        impl op::RpcMethod for $params {
            crate::operations::rpc_contract!($variant);
            fn params(&self) -> Result<Self, crate::error::PeerError> { Ok(self.clone()) }
            $(fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
                $validate(self, output)
            })?
        }
    };
}
contracts! {
    SearchList, "host/list/search" => (crate::conversation::records::SearchList, m::ThreadList) [clone],
    SubscribeList, "host/list/subscribe" => (crate::conversation::records::SubscribeList, crate::conversation::records::OpenedList),
    SubscribeCommandOutput, "host/conversation/output/subscribe" => (crate::conversation::records::SubscribeOutput, crate::conversation::records::OpenedOutput),
    SubscribeConversation, "host/conversation/subscribe" => (crate::conversation::records::Subscribe, crate::conversation::records::Opened),
    CreateConversation, "host/conversation/create" => (crate::conversation::records::Create, crate::conversation::records::Opened),
    ForkConversation, "host/conversation/fork" => (crate::conversation::records::Fork, s::SessionRef) [clone],
    ChangeConversation, "host/conversation/change" => (crate::conversation::records::Change, crate::conversation::records::Acknowledged) [clone],
    ExtendConversation, "host/conversation/extend" => (crate::conversation::records::Extend, crate::models::Empty) [clone],
    HydrateConversation, "host/conversation/hydrate" => (crate::conversation::records::Hydrate, crate::conversation::records::Hydrated) [clone],
    CheckConversationCommands, "host/conversation/receipts" => (crate::conversation::records::CheckCommands, crate::conversation::records::Receipts) [clone],
    ReadHandoffHistory, "host/session/context/read" => (crate::handoff::ReadHistory, crate::handoff::Page) [clone],
    UploadFeedback, "host/feedback/upload" => (crate::feedback::UploadFeedback, crate::feedback::FeedbackReceipt) [clone, crate::feedback::validate_receipt],
    MarkSessionRead, "host/session/markRead" => (crate::live_activity::MarkSessionRead, m::Empty) [clone],
    RegisterLiveActivity, "host/liveActivity/register" => (crate::live_activity::RegisterLiveActivity, crate::live_activity::LiveActivityRegistration) [clone],
    UnregisterLiveActivity, "host/liveActivity/unregister" => (crate::live_activity::UnregisterLiveActivity, m::Empty) [clone],
    RequestSession, "host/session/request" => (op::OpenRequest, s::SessionRef) [clone],
    AddProject, "host/project/add" => (op::AddProject, String) [clone],
    ScanProjects, "host/project/scan" => (crate::project_import::ScanProjects, crate::project_import::ProjectScan) [clone],
    ImportProjects, "host/project/import" => (crate::project_import::ImportProjects, crate::project_import::ProjectImportResult) [clone],
    CloneProject, "host/project/clone" => (crate::project_clone::CloneProject, crate::project_clone::ProjectClones) [clone],
    ReadProjectClones, "host/project/clones" => (crate::project_clone::ReadProjectClones, crate::project_clone::ProjectClones) [clone],
    ChangeProjectClone, "host/project/clone/change" => (crate::project_clone::ChangeProjectClone, crate::project_clone::ProjectClones) [clone],
    ReadListDecorations, "host/session/decorations/read" => (op::ReadListDecorations, op::ListDecorations) [clone],
    ListAgents, "host/session/agents" => (op::ListAgents, Vec<m::AgentObservation>) [clone],
    ReadPermissionSettings, "host/permissions/read" => (crate::permissions::ReadPermissionSettings, crate::permissions::PermissionSettings) [clone],
    UpdatePermissionSettings, "host/permissions/update" => (crate::permissions::UpdatePermissionSettings, crate::permissions::PermissionSettings) [clone],
    ComposerCatalog, "host/composer/catalog" => (op::LoadComposerCatalog, crate::composer::ComposerCatalog) [clone],
    ListModels, "host/model/list" => (op::ListModels, op::ModelPage) [clone],
    ReadModelSettings, "host/model/settings" => (op::ReadModelSettings, m::ModelSettings) [clone],
    ReadDshCredentials, "host/credential/dsh/read" => (crate::dsh::ReadCredentials, crate::dsh::Credentials) [clone],
    UpdateDshCredential, "host/credential/dsh/update" => (crate::dsh::KeyUpdate, crate::dsh::Credentials) [clone],
    Transcribe, "host/dictation/transcribe" => (op::Transcribe, op::Transcription),
    PrepareDictation, "host/dictation/prepare" => (op::DictationPreparation, m::Empty),
    CancelDictation, "host/dictation/cancel" => (op::DictationPreparation, m::Empty),
    ListFiles, "host/file/list" => (op::ListFiles, m::FileList) [clone],
    ListTreeEntries, "host/file/tree" => (crate::file_tree::ListTreeEntries, crate::file_tree::TreeEntries) [clone],
    SearchTreeEntries, "host/file/search" => (crate::file_tree::SearchTreeEntries, crate::file_tree::TreeEntries) [clone],
    ResolveFile, "host/file/resolve" => (op::ResolveFile, m::FileEntry) [clone],
    ReadFileMetadata, "host/file/metadata" => (crate::file_metadata::ReadFileMetadata, crate::file_metadata::FileMetadataEntries) [clone],
    ReadFileOpenCapabilities, "host/file/openCapabilities" => (crate::editors::ReadFileOpenCapabilities, crate::editors::FileOpenCapabilities) [clone],
    OpenInEditor, "host/file/openInEditor" => (crate::editors::OpenInEditor, m::Empty) [clone],
    ReadFile, "host/file/read" => (op::ListFiles, m::FileContent),
    WriteFile, "host/file/write" => (op::WriteFile, m::FileContent) [clone],
    WriteWorkspaceFile, "host/file/writeWorkspace" => (op::WriteWorkspaceFile, String) [clone],
    Upload, "host/blob/upload" => (op::Upload, m::TransferGrant),
    Download, "host/blob/download" => (op::ListFiles, m::TransferGrant),
    ReadVisualization, "host/visualize/read" => (op::LoadVisualization, String) [clone],
    ChangeWorkspace, "host/workspace/change" => (op::ChangeWorkspace, m::Empty) [clone],
    ReviewWorkspace, "host/workspace/review" => (op::ReviewWorkspace, m::WorkspaceReview) [clone],
    ReadWorktreeSettings, "host/worktree/settings/read" => (m::Empty, m::WorktreeSettings),
    UpdateWorktreeSettings, "host/worktree/settings/update" => (m::WorktreeSettings, m::WorktreeSettings),
    ListWorktrees, "host/worktree/list" => (m::Empty, Vec<m::Worktree>),
    RemoveWorktree, "host/worktree/remove" => (op::RemoveWorktree, ()) [clone],
    ListAccounts, "host/account/list" => (m::Empty, op::Accounts),
    ReadAccountUsage, "host/account/usage" => (op::ReadAccountUsage, op::AccountUsage) [clone],
    SelectAccount, "host/account/select" => (op::SelectAccount, op::AccountSelection) [clone],
    LogoutAccount, "host/account/logout" => (op::LogoutAccount, m::Empty) [clone],
    StartAccountLogin, "host/account/login/start" => (op::StartAccountLogin, op::AccountLogin) [clone],
    SubmitAccountLogin, "host/account/login/submit" => (op::SubmitAccountLogin, m::Empty) [clone],
    ReadAccountLogin, "host/account/login/status" => (op::ReadAccountLogin, op::AccountLoginStatus) [clone],
    CancelAccountLogin, "host/account/login/cancel" => (op::CancelAccountLogin, m::Empty) [clone],
    StartTerminal, "host/terminal/start" => (op::StartTerminal, m::Empty) [clone],
    StartAccountTerminal, "host/account/terminal/start" => (op::StartAccountTerminal, m::Empty) [clone],
    ResizeTerminal, "host/terminal/resize" => (op::ResizeTerminal, m::Empty) [clone],
    WriteTerminal, "host/terminal/write" => (op::TerminalWrite, m::Empty),
    DetachTerminal, "host/terminal/detach" => (op::DetachTerminal, m::Empty) [clone],
    KillTerminal, "host/terminal/kill" => (op::TerminalKill, m::Empty),
    Pair, "host/pair" => (op::Pair, m::Empty) [clone],
    IdentifyDevice, "host/device/identify" => (op::IdentifyDevice, op::HostIdentity) [clone],
    HostName, "host/name" => (m::Empty, String),
    HostStatus, "host/status" => (m::Empty, m::HostStatus),
    PrepareHostUpdate, "host/update/prepare" => (op::PrepareHostUpdate, op::HostUpdate) [clone],
    Invite, "host/invite" => (m::Empty, m::Invitation),
    ListRemotes, "host/listRemotes" => (m::Empty, Vec<m::RemoteHost>),
    RegisterRemote, "host/registerRemote" => (op::RegisterRemoteHost, m::RemoteHost) [clone],
    RemoveRemote, "host/removeRemote" => (op::RemoveRemoteHost, m::Empty) [clone],
    Revoke, "host/revoke" => (op::RevokeDevice, m::Empty) [clone],
    Browser, "host/browser" => (crate::browser::BrowserRequest, crate::browser::BrowserFrame) [clone],
    ConnectionPerformance, "host/diagnostics/connection" => (crate::diagnostics::ConnectionPerformance, m::Empty) [clone],
    CodexCli, "host/codex/cli" => (op::CodexCliRequest, op::CodexCliStatus) [clone],
    ReadProviderSettings, "host/provider/settings" => (m::Empty, crate::providers::ProviderSettings),
    ReadHostSoftware, "host/software" => (m::Empty, op::HostSoftware),
    SetProviderEnabled, "host/provider/enable" => (crate::providers::SetProviderEnabled, crate::providers::ProviderSettings) [clone],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_input_does_not_merge_file_read_and_download() {
        let params = serde_json::json!({"path":"/workspace/file"});
        let read = from_json("host/file/read", params.clone()).unwrap();
        let download = from_json("host/blob/download", params).unwrap();
        assert!(matches!(
            decode::<Call>(&encode(&read).unwrap()).unwrap(),
            Call::ReadFile(_)
        ));
        assert!(matches!(
            decode::<Call>(&encode(&download).unwrap()).unwrap(),
            Call::Download(_)
        ));
        assert_eq!(read.params_json().unwrap(), download.params_json().unwrap());
        assert_ne!(read.method(), download.method());
    }
}
