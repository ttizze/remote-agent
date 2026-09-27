//! One contract per native request; provider JSON conversion stays at its boundary.
use super::{json_boundary::Opaque, *};
use crate::{models as m, operations as op, session as s};
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
            /// Thread and optional in-flight input identity. Host uses this instead of
            /// re-matching the operation table; clients never choose a route from it.
            pub fn session_scope(&self) -> Option<(&str, Option<&str>)> {
                match self {
                    Self::Submit(p) => Some((
                        p.thread_id.as_str(),
                        Some(p.client_user_message_id.as_str()),
                    )),
                    Self::StartTurn(p) => Some((
                        p.thread_id.as_str(),
                        Some(p.client_user_message_id.as_str()),
                    )),
                    Self::SteerTurn(p) => Some((
                        p.thread_id.as_str(),
                        Some(p.client_user_message_id.as_str()),
                    )),
                    Self::QueueTurn(p) => Some((
                        p.thread_id.as_str(),
                        Some(p.client_user_message_id.as_str()),
                    )),
                    Self::ResumeThread(p) => Some((p.thread_id.as_str(), None)),
                    Self::ForkThread(p) => Some((p.thread_id.as_str(), None)),
                    Self::Interrupt(p) => Some((p.thread_id.as_str(), None)),
                    Self::ReadItem(p) => Some((p.thread_id.as_str(), None)),
                    Self::RenameThread(p) => Some((p.thread_id.as_str(), None)),
                    _ => None,
                }
            }
        }
        pub fn from_json(method: &str, params: Value) -> io::Result<Call> {
            Ok(match method {
                $($method => Call::$variant(serde_json::from_value(params).map_err(io::Error::other)?)),*,
                _ => Call::Provider(ProviderCall { method: method.into(), params }),
            })
        }
        pub fn provider_response(method: &str, line: &str) -> Result<Response, crate::message::RpcMessageError> {
            match method {
                $($method => Response::from_result(crate::message::RpcResponse::<$result>::parse(line)?.outcome)),*,
                _ => Response::from_result(crate::message::RpcResponse::<Value>::parse(line)?.outcome),
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
    OpenSession, "host/session/open" => (s::OpenSession, s::OpenedSession),
    AnswerSession, "host/session/answer" => (op::SessionAnswer, m::Empty),
    RequestSession, "host/session/request" => (op::OpenRequest, s::SessionRef) [clone],
    SessionScope, "host/session/scope" => (m::Empty, String),
    ReadItem, "host/thread/item/read" => (op::ReadItem, op::ItemResponse) [clone, op::ReadItem::validate],
    AddProject, "host/project/add" => (op::AddProject, String) [clone],
    ListThreads, "host/thread/list" => (op::ListThreads, m::ThreadList) [clone],
    StartThread, "host/thread/start" => (op::StartThread, m::ThreadResponse) [clone, op::StartThread::validate],
    ForkThread, "thread/fork" => (op::ForkThread, m::ThreadResponse) [clone, op::ForkThread::validate],
    ResumeThread, "thread/resume" => (op::ResumeThread, m::Empty) [clone],
    Submit, "host/session/submit" => (op::Submission, op::SubmissionReceipt) [clone],
    StartTurn, "turn/start" => (op::StartTurn, op::StartedTurn) [clone, op::StartTurn::validate],
    SteerTurn, "turn/steer" => (op::SteerTurn, m::Empty) [clone],
    QueueTurn, "thread/queue/add" => (op::QueueTurn, op::QueuedTurn) [clone, op::QueueTurn::validate],
    Interrupt, "turn/interrupt" => (op::Interrupt, m::Empty) [clone],
    ComposerCatalog, "host/composer/catalog" => (op::LoadComposerCatalog, crate::composer::ComposerCatalog) [clone],
    ListModels, "model/list" => (op::ListModels, op::ModelPage) [clone],
    Transcribe, "host/dictation/transcribe" => (op::Transcribe, op::Transcription),
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
    ResizeTerminal, "process/resizePty" => (op::ResizeTerminal, m::Empty) [clone],
    WriteTerminal, "process/writeStdin" => (op::TerminalWrite, m::Empty),
    DetachTerminal, "host/terminal/detach" => (op::DetachTerminal, m::Empty) [clone],
    KillTerminal, "process/kill" => (op::TerminalKill, m::Empty),
    Pair, "host/pair" => (op::Pair, m::Empty) [clone],
    HostName, "host/name" => (m::Empty, String),
    HostStatus, "host/status" => (m::Empty, m::HostStatus),
    Invite, "host/invite" => (m::Empty, m::Invitation),
    ListRemotes, "host/listRemotes" => (m::Empty, Vec<m::RemoteHost>),
    RegisterRemote, "host/registerRemote" => (op::RegisterRemoteHost, m::RemoteHost) [clone],
    RemoveRemote, "host/removeRemote" => (op::RemoveRemoteHost, m::Empty) [clone],
    Revoke, "host/revoke" => (op::RevokeDevice, m::Empty) [clone],
    RenameThread, "thread/name/set" => (op::RenameThread, m::Empty) [clone],
    Provider, "provider" => (ProviderCall, Opaque),
    Browser, "host/browser" => (crate::browser::BrowserRequest, crate::browser::BrowserFrame) [clone],
    ConnectionPerformance, "host/diagnostics/connection" => (crate::diagnostics::ConnectionPerformance, m::Empty) [clone],
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operations::RpcMethod;

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
        let start: op::StartTurn = serde_json::from_value(params.clone()).unwrap();
        let queue: op::QueueTurn = serde_json::from_value(params).unwrap();
        for id in ["", "  ", "turn"] {
            let started = op::StartedTurn {
                turn: op::TurnIdentity { id: id.into() },
            };
            let queued = op::QueuedTurn {
                queued_submission: op::TurnIdentity { id: id.into() },
            };
            assert_eq!(RpcMethod::validate(&start, &started).is_ok(), id == "turn");
            assert_eq!(RpcMethod::validate(&queue, &queued).is_ok(), id == "turn");
        }
    }

    #[test]
    fn session_scope_is_owned_by_the_operation_table() {
        let submit = from_json(
            "host/session/submit",
            serde_json::json!({
                "threadId":"thread",
                "clientUserMessageId":"input",
                "input":[]
            }),
        )
        .unwrap();
        assert_eq!(submit.session_scope(), Some(("thread", Some("input"))));
        let interrupt = from_json(
            "turn/interrupt",
            serde_json::json!({"threadId":"thread","turnId":"turn"}),
        )
        .unwrap();
        assert_eq!(interrupt.session_scope(), Some(("thread", None)));
        let files = from_json("host/file/list", serde_json::json!({"path":"/"})).unwrap();
        assert_eq!(files.session_scope(), None);
        let start_thread = from_json("host/thread/start", serde_json::json!({"cwd":"/"})).unwrap();
        assert_eq!(start_thread.session_scope(), None);
    }
}
