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
    };
    (@clone $params:ty, $variant:ident $(, $validate:path)?) => {
        impl c::RpcMethod for $params {
            crate::client::rpc_contract!($variant);
            fn params(&self) -> Result<Self, crate::peer::PeerError> { Ok(self.clone()) }
            $(fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
                $validate(self, output)
            })?
        }
    };
}
contracts! {
    OpenSession, "host/session/open" => (s::OpenSession, s::OpenedSession),
    AnswerSession, "host/session/answer" => (c::SessionAnswer, m::Empty),
    RequestSession, "host/session/request" => (c::OpenRequest, s::SessionRef) [clone],
    SessionScope, "host/session/scope" => (m::Empty, String),
    ReadItem, "host/thread/item/read" => (c::ReadItem, c::ItemResponse) [clone, c::ReadItem::validate],
    AddProject, "host/project/add" => (c::AddProject, String) [clone],
    ListThreads, "host/thread/list" => (c::ListThreads, m::ThreadList) [clone],
    StartThread, "host/thread/start" => (op::StartThread, m::ThreadResponse) [clone, op::StartThread::validate],
    ForkThread, "thread/fork" => (op::ForkThread, m::ThreadResponse) [clone, op::ForkThread::validate],
    ResumeThread, "thread/resume" => (c::ResumeThread, m::Empty) [clone],
    StartTurn, "turn/start" => (c::StartTurn, c::StartedTurn) [clone, c::StartTurn::validate],
    SteerTurn, "turn/steer" => (c::SteerTurn, m::Empty) [clone],
    QueueTurn, "thread/queue/add" => (c::QueueTurn, c::QueuedTurn) [clone, c::QueueTurn::validate],
    Interrupt, "turn/interrupt" => (op::Interrupt, m::Empty) [clone],
    ComposerCatalog, "host/composer/catalog" => (op::LoadComposerCatalog, crate::composer::ComposerCatalog) [clone],
    ListModels, "model/list" => (c::ListModels, c::ModelPage) [clone],
    Transcribe, "host/dictation/transcribe" => (c::Transcribe, c::Transcription),
    ListFiles, "host/file/list" => (op::ListFiles, m::FileList) [clone],
    ReadFile, "host/file/read" => (op::ListFiles, m::FileContent),
    WriteFile, "host/file/write" => (c::WriteFile, m::FileContent) [clone],
    Upload, "host/blob/upload" => (c::Upload, m::TransferGrant),
    Download, "host/blob/download" => (op::ListFiles, m::TransferGrant),
    ReadVisualization, "host/visualize/read" => (c::LoadVisualization, String) [clone],
    ReviewWorkspace, "host/workspace/review" => (c::ReviewWorkspace, m::WorkspaceReview) [clone],
    ReadWorktreeSettings, "host/worktree/settings/read" => (m::Empty, m::WorktreeSettings),
    UpdateWorktreeSettings, "host/worktree/settings/update" => (m::WorktreeSettings, m::WorktreeSettings),
    ListWorktrees, "host/worktree/list" => (m::Empty, Vec<m::Worktree>),
    RemoveWorktree, "host/worktree/remove" => (c::RemoveWorktree, ()) [clone],
    ListAccounts, "host/account/list" => (m::Empty, c::Accounts),
    ReadAccountUsage, "host/account/usage" => (op::ReadAccountUsage, c::AccountUsage) [clone],
    SelectAccount, "host/account/select" => (c::SelectAccount, c::AccountSelection) [clone],
    LogoutAccount, "host/account/logout" => (c::LogoutAccount, m::Empty) [clone],
    StartAccountLogin, "host/account/login/start" => (c::StartAccountLogin, c::AccountLogin) [clone],
    SubmitAccountLogin, "host/account/login/submit" => (c::SubmitAccountLogin, m::Empty) [clone],
    ReadAccountLogin, "host/account/login/status" => (c::ReadAccountLogin, c::AccountLoginStatus) [clone],
    CancelAccountLogin, "host/account/login/cancel" => (c::CancelAccountLogin, m::Empty) [clone],
    StartTerminal, "host/terminal/start" => (c::StartTerminal, m::Empty) [clone],
    ResizeTerminal, "process/resizePty" => (op::ResizeTerminal, m::Empty) [clone],
    WriteTerminal, "process/writeStdin" => (c::TerminalWrite, m::Empty),
    DetachTerminal, "host/terminal/detach" => (op::DetachTerminal, m::Empty) [clone],
    KillTerminal, "process/kill" => (c::TerminalKill, m::Empty),
    Pair, "host/pair" => (c::Pair, m::Empty) [clone],
    HostStatus, "host/status" => (m::Empty, m::HostStatus),
    Invite, "host/invite" => (m::Empty, m::Invitation),
    ListRemotes, "host/listRemotes" => (m::Empty, Vec<m::RemoteHost>),
    RegisterRemote, "host/registerRemote" => (c::RegisterRemoteHost, m::RemoteHost) [clone],
    RemoveRemote, "host/removeRemote" => (op::RemoveRemoteHost, m::Empty) [clone],
    Revoke, "host/revoke" => (op::RevokeDevice, m::Empty) [clone],
    RenameThread, "thread/name/set" => (c::RenameThread, m::Empty) [clone],
    Provider, "provider" => (ProviderCall, Opaque),
    Browser, "host/browser" => (crate::browser::BrowserRequest, crate::browser::BrowserFrame) [clone],
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::RpcMethod;

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

    #[test]
    fn generated_submission_contracts_keep_response_validation() {
        let params =
            serde_json::json!({"threadId":"thread","clientUserMessageId":"input","input":[]});
        let start: c::StartTurn = serde_json::from_value(params.clone()).unwrap();
        let queue: c::QueueTurn = serde_json::from_value(params).unwrap();
        for id in ["", "  ", "turn"] {
            let started = c::StartedTurn {
                turn: c::TurnIdentity { id: id.into() },
            };
            let queued = c::QueuedTurn {
                queued_submission: c::TurnIdentity { id: id.into() },
            };
            assert_eq!(RpcMethod::validate(&start, &started).is_ok(), id == "turn");
            assert_eq!(RpcMethod::validate(&queue, &queued).is_ok(), id == "turn");
        }
    }
}
