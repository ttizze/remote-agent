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
        pub enum Call { $($variant($params)),* }
        impl Call {
            pub fn method(&self) -> &str {
                match self { $(Self::$variant(_) => $method),* }
            }
            pub fn params_json(&self) -> Result<Value, serde_json::Error> {
                match self { $(Self::$variant(params) => serde_json::to_value(params)),* }
            }
        }
        pub fn from_json(method: &str, params: Value) -> io::Result<Call> {
            Ok(match method {
                $($method => Call::$variant(serde_json::from_value(params).map_err(io::Error::other)?)),*,
                _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("unregistered method: {method}"))),
            })
        }
        pub fn provider_response(method: &str, line: &str) -> Result<Response, crate::message::RpcMessageError> {
            match method {
                $($method => super::json_boundary::typed_response::<$result>(line)),*,
                _ => Err(crate::message::RpcMessageError::InvalidCombination {reason:"unregistered BEX response"}),
            }
        }
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
    RegisterLiveActivity, "host/session/liveActivity/register" => (crate::live_activity::RegisterLiveActivity, crate::live_activity::LiveActivityRegistration) [clone],
    UnregisterLiveActivity, "host/session/liveActivity/unregister" => (crate::live_activity::UnregisterLiveActivity, m::Empty) [clone],
    OpenSession, "host/session/open" => (s::OpenSession, s::OpenedSession),
    ReadHistory, "host/session/history/read" => (s::ReadHistory, s::HistoryPage) [clone],
    ReadTurnItems, "host/session/turn/items" => (s::ReadTurnItems, m::Empty) [clone],
    AnswerSession, "host/session/answer" => (op::SessionAnswer, m::Empty),
    RequestSession, "host/session/request" => (op::OpenRequest, s::SessionRef) [clone],
    SessionScope, "host/session/scope" => (m::Empty, String),
    ReadItem, "host/session/item/read" => (op::ReadItem, op::ItemResponse) [clone, op::ReadItem::validate],
    AddProject, "host/project/add" => (op::AddProject, String) [clone],
    ListSessions, "host/session/list" => (op::ListSessions, m::ThreadList) [clone],
    CreateSession, "host/session/create" => (op::CreateSession, s::OpenedSession),
    ForkSession, "host/session/fork" => (op::ForkSession, m::ThreadResponse) [clone, op::ForkSession::validate],
    Submit, "host/session/submit" => (op::Submission, op::SubmissionReceipt) [clone],
    Interrupt, "host/session/interrupt" => (op::Interrupt, m::Empty) [clone],
    ReadPermissionSettings, "host/permissions/read" => (crate::permissions::ReadPermissionSettings, crate::permissions::PermissionSettings) [clone],
    UpdatePermissionSettings, "host/permissions/update" => (crate::permissions::UpdatePermissionSettings, crate::permissions::PermissionSettings) [clone],
    ComposerCatalog, "host/composer/catalog" => (op::LoadComposerCatalog, crate::composer::ComposerCatalog) [clone],
    ListModels, "host/model/list" => (op::ListModels, op::ModelPage) [clone],
    Transcribe, "host/dictation/transcribe" => (op::Transcribe, op::Transcription),
    PrepareDictation, "host/dictation/prepare" => (op::DictationPreparation, m::Empty),
    CancelDictation, "host/dictation/cancel" => (op::DictationPreparation, m::Empty),
    ListFiles, "host/file/list" => (op::ListFiles, m::FileList) [clone],
    ReadFile, "host/file/read" => (op::ListFiles, m::FileContent),
    WriteFile, "host/file/write" => (op::WriteFile, m::FileContent) [clone],
    Upload, "host/blob/upload" => (op::Upload, m::TransferGrant),
    Download, "host/blob/download" => (op::ListFiles, m::TransferGrant),
    ReadVisualization, "host/visualize/read" => (op::LoadVisualization, String) [clone],
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
    ResizeTerminal, "host/terminal/resize" => (op::ResizeTerminal, m::Empty) [clone],
    WriteTerminal, "host/terminal/write" => (op::TerminalWrite, m::Empty),
    DetachTerminal, "host/terminal/detach" => (op::DetachTerminal, m::Empty) [clone],
    KillTerminal, "host/terminal/kill" => (op::TerminalKill, m::Empty),
    Pair, "host/pair" => (op::Pair, m::Empty) [clone],
    HostName, "host/name" => (m::Empty, String),
    HostStatus, "host/status" => (m::Empty, m::HostStatus),
    Invite, "host/invite" => (m::Empty, m::Invitation),
    ListRemotes, "host/listRemotes" => (m::Empty, Vec<m::RemoteHost>),
    RegisterRemote, "host/registerRemote" => (op::RegisterRemoteHost, m::RemoteHost) [clone],
    RemoveRemote, "host/removeRemote" => (op::RemoveRemoteHost, m::Empty) [clone],
    Revoke, "host/revoke" => (op::RevokeDevice, m::Empty) [clone],
    RenameSession, "host/session/rename" => (op::RenameSession, m::Empty) [clone],
    Browser, "host/browser" => (crate::browser::BrowserRequest, crate::browser::BrowserFrame) [clone],
    ConnectionPerformance, "host/diagnostics/connection" => (crate::diagnostics::ConnectionPerformance, m::Empty) [clone],
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
