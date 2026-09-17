//! One contract per native request; provider JSON conversion stays at its boundary.
use super::json_boundary::Opaque;
use super::*;
use crate::{client as c, models as m, session as s, state::operations as op};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCall {
    pub method: String,
    #[serde(with = "super::json")]
    pub params: Value,
}
macro_rules! contracts {
    ($($variant:ident, $method:literal => ($params:ty, $result:ty)),* $(,)?) => {
        #[derive(Debug, Clone, Serialize, Deserialize)]
        pub enum Call { $($variant($params)),* }
        impl Call {
            pub fn method(&self) -> &str {
                if let Self::Provider(call) = self { return &call.method; }
                match self { $(Self::$variant(_) => $method),* }
            }
            pub fn params_json(&self) -> Result<Value, serde_json::Error> {
                if let Self::Provider(call) = self { return Ok(call.params.clone()); }
                match self { $(Self::$variant(params) => serde_json::to_value(params)),* }
            }
        }
        pub fn from_json(method: &str, params: Value) -> io::Result<Call> {
            Ok(match method {
                $($method => Call::$variant(serde_json::from_value(params).map_err(io::Error::other)?)),*,
                _ => Call::Provider(ProviderCall { method: method.into(), params }),
            })
        }
        pub fn provider_response(method: &str, line: &str) -> Result<Response, crate::peer::RpcMessageError> {
            match method {
                $($method => Response::from_result(crate::peer::RpcResponse::<$result>::parse(line)?.outcome)),*,
                _ => Response::from_result(crate::peer::RpcResponse::<Value>::parse(line)?.outcome),
            }
        }
        pub fn fixture_reply(method: &str, bytes: &[u8]) -> io::Result<Value> {
            match method {
                $($method => Ok(decode::<Response<$result>>(bytes)?.into_value())),*,
                _ => Ok(decode::<Response<Opaque>>(bytes)?.into_value()),
            }
        }
    }
}
contracts! {
    OpenSession, "host/session/open" => (s::OpenSession, s::OpenedSession),
    AnswerSession, "host/session/answer" => (c::SessionAnswer, m::Empty),
    RequestSession, "host/session/request" => (c::OpenRequest, s::SessionRef),
    SessionScope, "host/session/scope" => (m::Empty, String),
    ReadItem, "host/thread/item/read" => (c::ReadItem, c::ItemResponse),
    ListThreads, "host/thread/list" => (c::ListThreads, m::ThreadList),
    StartThread, "host/thread/start" => (op::StartThread, m::ThreadResponse),
    ForkThread, "thread/fork" => (op::ForkThread, m::ThreadResponse),
    ResumeThread, "thread/resume" => (c::ResumeThread, m::Empty),
    StartTurn, "turn/start" => (c::StartTurn, c::StartedTurn),
    SteerTurn, "turn/steer" => (c::SteerTurn, m::Empty),
    QueueTurn, "thread/queue/add" => (c::QueueTurn, c::QueuedTurn),
    Interrupt, "turn/interrupt" => (op::Interrupt, m::Empty),
    ListModels, "model/list" => (c::ListModels, c::ModelPage),
    Transcribe, "host/dictation/transcribe" => (c::Transcribe, c::Transcription),
    ListFiles, "host/file/list" => (op::ListFiles, m::FileList),
    ReadFile, "host/file/read" => (op::ListFiles, m::FileContent),
    WriteFile, "host/file/write" => (c::WriteFile, m::FileContent),
    Upload, "host/blob/upload" => (c::Upload, m::TransferGrant),
    Download, "host/blob/download" => (op::ListFiles, m::TransferGrant),
    ReadVisualization, "host/visualize/read" => (c::LoadVisualization, String),
    ReviewWorkspace, "host/workspace/review" => (c::ReviewWorkspace, m::WorkspaceReview),
    ReadWorktreeSettings, "host/worktree/settings/read" => (m::Empty, m::WorktreeSettings),
    UpdateWorktreeSettings, "host/worktree/settings/update" => (m::WorktreeSettings, m::WorktreeSettings),
    ListWorktrees, "host/worktree/list" => (m::Empty, Vec<m::Worktree>),
    RemoveWorktree, "host/worktree/remove" => (c::RemoveWorktree, ()),
    ListAccounts, "host/account/list" => (m::Empty, c::Accounts),
    SelectAccount, "host/account/select" => (c::SelectAccount, c::AccountSelection),
    LogoutAccount, "host/account/logout" => (c::LogoutAccount, m::Empty),
    StartAccountLogin, "host/account/login/start" => (m::Empty, c::AccountLogin),
    ReadAccountLogin, "host/account/login/status" => (c::ReadAccountLogin, c::AccountLoginStatus),
    CancelAccountLogin, "host/account/login/cancel" => (c::CancelAccountLogin, m::Empty),
    StartTerminal, "host/terminal/start" => (c::StartTerminal, m::Empty),
    ResizeTerminal, "process/resizePty" => (op::ResizeTerminal, m::Empty),
    WriteTerminal, "process/writeStdin" => (c::TerminalWrite, m::Empty),
    DetachTerminal, "host/terminal/detach" => (op::DetachTerminal, m::Empty),
    KillTerminal, "process/kill" => (c::TerminalKill, m::Empty),
    Pair, "host/pair" => (c::Pair, m::Empty),
    HostStatus, "host/status" => (m::Empty, m::HostStatus),
    Invite, "host/invite" => (m::Empty, m::Invitation),
    ListRemotes, "host/listRemotes" => (m::Empty, Vec<m::RemoteHost>),
    RegisterRemote, "host/registerRemote" => (c::RegisterRemoteHost, m::RemoteHost),
    RemoveRemote, "host/removeRemote" => (op::RemoveRemoteHost, m::Empty),
    Revoke, "host/revoke" => (op::RevokeDevice, m::Empty),
    Provider, "provider" => (ProviderCall, Opaque),
}
