//! Shared request records and their typed RPC contracts.
use crate::{error::PeerError, models::ThreadResponse};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

/// The method, parameters and result are one contract.
pub trait RpcMethod: Serialize {
    type Contract: crate::protocol::contracts::Contract<Output = Self::Output>;
    type Output: DeserializeOwned + Serialize;
    fn method(&self) -> &'static str {
        <Self::Contract as crate::protocol::contracts::Contract>::METHOD
    }
    fn params(
        &self,
    ) -> Result<<Self::Contract as crate::protocol::contracts::Contract>::Params, PeerError>;
    fn request(&self) -> Result<crate::protocol::Call, PeerError> {
        self.params()
            .map(<Self::Contract as crate::protocol::contracts::Contract>::call)
    }
    fn subscription(_output: &mut Self::Output, _id: uuid::Uuid) {}
    fn validate(&self, _output: &Self::Output) -> Result<(), &'static str> {
        Ok(())
    }
}
#[macro_export]
macro_rules! rpc_contract {
    ($variant:ident) => {
        type Contract = $crate::protocol::contracts::$variant;
        type Output = <$crate::protocol::contracts::$variant as $crate::protocol::contracts::Contract>::Output;
    };
}
pub use crate::rpc_contract;
#[macro_export]
macro_rules! rpc_method {
    ($name:ty, $variant:ident, |$this:ident| $params:expr) => {
        impl $crate::operations::RpcMethod for $name {
            $crate::operations::rpc_contract!($variant);
            fn params(&$this) -> Result<<Self::Contract as $crate::protocol::contracts::Contract>::Params, $crate::error::PeerError> {
                Ok($params)
            }
        }
    };
}
pub use crate::rpc_method;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexCliRequest {
    pub update: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexCliStatus {
    pub running: bool,
    pub version: Option<String>,
    pub restart_required: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pair {
    pub invitation: uuid::Uuid,
}

/// The first request of every connection: the device names itself and learns
/// what it needs before trusting cached state from an earlier connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentifyDevice {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostIdentity {
    /// Identifies the native storage the Host's providers read.
    pub storage_scope: String,
    /// The Host's currently reachable addresses for the next cold connection.
    pub ticket: String,
    /// The authenticated Host's actual home, used for lexical file labels and paths.
    pub home_directory: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct ReadHostStatus {}
rpc_method!(ReadHostStatus, HostStatus, |self| crate::models::Empty {});

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateInvitation {}
rpc_method!(CreateInvitation, Invite, |self| crate::models::Empty {});

/// Local updater only: persist interrupted work before shutting down this Host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrepareHostUpdate {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostUpdate {
    pub executable: String,
    pub arguments: Vec<String>,
    pub working_directory: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostSoftware {
    pub version: String,
    pub revision: String,
    pub build: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadHostSoftware {}
rpc_method!(ReadHostSoftware, ReadHostSoftware, |self| {
    crate::models::Empty {}
});

#[derive(Debug, Serialize, Clone)]
pub struct ListRemoteHosts {}
rpc_method!(ListRemoteHosts, ListRemotes, |self| crate::models::Empty {});
#[derive(Debug, Serialize, Clone, Deserialize)]
pub struct RegisterRemoteHost {
    pub ticket: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Input {
    Context {
        context: Box<crate::context::MessageContext>,
    },
    Text {
        text: String,
    },
    Skill {
        name: String,
        path: String,
    },
    LocalImage {
        path: String,
    },
    Mention {
        path: String,
        name: String,
    },
}
impl Input {
    pub fn contexts(input: &[Self]) -> impl Iterator<Item = &crate::context::MessageContext> {
        input.iter().filter_map(|part| match part {
            Self::Context { context } => Some(context.as_ref()),
            _ => None,
        })
    }
    pub fn validate_contexts(input: &[Self]) -> Result<(), &'static str> {
        crate::context::validate_contexts(Self::contexts(input))
    }
    pub fn message_parts(input: &[Self]) -> Vec<crate::items::MessagePart> {
        use crate::items::MessagePart;
        input
            .iter()
            .map(|part| match part {
                Self::Context { context } => MessagePart::Context {
                    context: context.clone(),
                },
                Self::Text { text } => MessagePart::Text { text: text.clone() },
                Self::LocalImage { path } => MessagePart::Image {
                    source: path.clone(),
                },
                Self::Skill { name, path } => MessagePart::Invocation {
                    name: name.clone(),
                    path: path.clone(),
                },
                Self::Mention { name, path }
                    if path.starts_with("plugin://") || path.starts_with("app://") =>
                {
                    MessagePart::Invocation {
                        name: name.clone(),
                        path: path.clone(),
                    }
                }
                Self::Mention { name, path } => MessagePart::Attachment {
                    name: name.clone(),
                    path: path.clone(),
                },
            })
            .collect()
    }
}

pub fn validate_thread(
    output: &ThreadResponse,
    expected: Option<&crate::session::SessionRef>,
) -> Result<(), &'static str> {
    let id = output
        .thread
        .id
        .as_ref()
        .filter(|id| id.validate().is_ok())
        .ok_or("thread ID is missing")?;
    if expected.is_some_and(|expected| id != expected) {
        Err("thread ID does not match")
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemResponse {
    pub item: crate::models::Item,
    pub transfer: Option<crate::models::TransferGrant>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameSession {
    pub thread_id: crate::session::SessionRef,
    pub name: String,
}

#[derive(Debug, Serialize, Clone, Deserialize)]
pub struct ListModels {
    pub provider: crate::session::ProviderKind,
    pub limit: usize,
    pub cursor: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadModelSettings {
    pub model: crate::models::ModelRef,
}
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPage {
    pub data: Vec<crate::models::Model>,
    pub next_cursor: Option<String>,
    #[serde(default)]
    pub provider_errors: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound(serialize = "T: AsRef<[u8]>", deserialize = "T: From<Vec<u8>>"))]
pub struct Transcribe<T = Vec<u8>> {
    pub preparation: Option<String>,
    #[serde(with = "crate::protocol::bytes")]
    pub audio: T,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Transcription {
    pub text: String,
}
impl<T: AsRef<[u8]>> RpcMethod for Transcribe<T> {
    crate::operations::rpc_contract!(Transcribe);
    fn params(
        &self,
    ) -> Result<<Self::Contract as crate::protocol::contracts::Contract>::Params, PeerError> {
        Ok(Transcribe {
            preparation: self.preparation.clone(),
            audio: self.audio.as_ref().to_vec(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DictationPreparation {
    pub id: String,
}

#[derive(Debug, Serialize, Clone, Deserialize)]
pub struct WriteFile {
    pub path: String,
    pub revision: String,
    pub text: String,
}

/// Writes `text` to `relative_path` in the workspace at `cwd`, making its
/// folders and replacing a file already there, as T3 Code saves a plan to
/// the workspace; the path stays inside the workspace. Replies with the
/// workspace-relative path written.
#[derive(Debug, Serialize, Clone, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct WriteWorkspaceFile {
    pub cwd: String,
    pub relative_path: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: String,
    pub provider: crate::session::ProviderKind,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub usage: Option<AccountUsage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountUsage {
    pub windows: Vec<UsageWindow>,
    pub fetched_at: i64,
    pub error: Option<String>,
}

/// Provider facts; clients format the period in the selected UI language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum UsagePeriod {
    Minutes { minutes: u64 },
    Monthly,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    pub period: UsagePeriod,
    pub name: Option<String>,
    pub remaining_percent: u32,
    pub resets_at: Option<i64>,
}

impl UsageWindow {
    pub fn from_used(
        period: UsagePeriod,
        name: Option<String>,
        used: f64,
        resets_at: Option<i64>,
    ) -> Option<Self> {
        used.is_finite().then(|| Self {
            period,
            name,
            remaining_percent: (100.0 - used.clamp(0.0, 100.0)).floor() as u32,
            resets_at,
        })
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Accounts {
    pub accounts: Vec<Account>,
    pub selected: std::collections::HashMap<crate::session::ProviderKind, String>,
    #[serde(default)]
    pub errors: std::collections::HashMap<crate::session::ProviderKind, String>,
}

impl Accounts {
    pub fn is_selected(&self, account: &Account) -> bool {
        self.selected.get(&account.provider) == Some(&account.id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartAccountLogin {
    pub provider: crate::session::ProviderKind,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SubmitAccountLogin {
    pub provider: crate::session::ProviderKind,
    pub id: String,
    pub code: String,
}
impl std::fmt::Debug for SubmitAccountLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubmitAccountLogin")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSelection {
    pub provider: crate::session::ProviderKind,
    pub selected_id: String,
    pub persistence_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountLogin {
    pub provider: crate::session::ProviderKind,
    pub login_id: String,
    pub requires_code_submission: bool,
    pub user_code: String,
    pub verification_url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountLoginStatus {
    pub completed: bool,
    pub account_id: Option<String>,
}

use crate::requests::Answer;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}

// Shared Host/Client request records; Store behavior lives in state::operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddProject {
    pub cwd: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ListSessions {
    pub query: crate::models::ListQuery,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListProjectSessions {
    pub project_id: String,
    pub limit: u32,
    pub search_term: String,
}

/// Values already obtained by the title read; decorating them must not list
/// provider sessions or read conversation bodies again.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ListDecorationScope {
    Root {
        part: Option<crate::models::ListPart>,
    },
    Project {
        project_id: String,
    },
    Task {
        session: crate::session::SessionRef,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadListDecorations {
    pub scope: ListDecorationScope,
    pub projects: Vec<(String, Vec<String>)>,
    pub threads: Vec<(crate::session::SessionRef, String, Option<String>)>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListDecorations {
    pub icons: std::collections::BTreeMap<String, Option<crate::models::ProjectIcon>>,
    pub statuses: Vec<(crate::session::SessionRef, Option<bool>)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListAgents {
    pub thread_id: crate::session::SessionRef,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadItem {
    pub thread_id: crate::session::SessionRef,
    pub turn_id: crate::ids::TurnId,
    pub item_id: crate::ids::ItemId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenRequest {
    pub request_id: crate::ids::RequestId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartTerminal {
    pub command: Option<String>,
    #[serde(rename = "processHandle")]
    pub handle: String,
    pub cwd: String,
    pub size: TerminalSize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartAccountTerminal {
    pub handle: String,
    pub provider: crate::session::ProviderKind,
    pub id: String,
    pub size: TerminalSize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewWorkspace {
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub include_options: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeWorkspace {
    pub cwd: String,
    pub branch: String,
    pub action: WorkspaceAction,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorkspaceAction {
    SwitchBranch {
        name: String,
    },
    CreateBranch {
        name: String,
    },
    Push {
        remote: String,
    },
    SaveScript {
        previous_name: Option<String>,
        name: String,
        command: String,
    },
    RemoveScript {
        name: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoveWorktree {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectAccount {
    pub provider: crate::session::ProviderKind,
    #[serde(rename = "accountId")]
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogoutAccount {
    pub provider: crate::session::ProviderKind,
    #[serde(rename = "accountId")]
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadAccountLogin {
    pub provider: crate::session::ProviderKind,
    #[serde(rename = "loginId")]
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelAccountLogin {
    pub provider: crate::session::ProviderKind,
    #[serde(rename = "loginId")]
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadVisualization {
    pub path: String,
    pub cwd: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Upload {
    pub file_name: String,
    pub size: u64,
    pub sha256: [u8; 32],
}
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TerminalWrite {
    pub process_handle: String,
    #[serde(rename = "deltaBase64", with = "crate::protocol::bytes")]
    pub data: Vec<u8>,
}
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TerminalKill {
    pub process_handle: String,
}
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SessionAnswer {
    pub request_id: crate::ids::RequestId,
    pub answer: Answer,
}

/// Shared xterm palette, also used for Host-owned terminal-query replies.
pub fn terminal_color(index: u16) -> u32 {
    const PALETTE: [u32; 16] = [
        0x181818, 0xcc6666, 0xb5bd68, 0xf0c674, 0x81a2be, 0xb294bb, 0x8abeb7, 0xc5c8c6, 0x666666,
        0xd54e53, 0xb9ca4a, 0xe7c547, 0x7aa6da, 0xc397d8, 0x70c0b1, 0xeaeaea,
    ];
    let index = u32::from(index);
    match index {
        0..=15 => PALETTE[index as usize],
        16..=231 => {
            let n = index - 16;
            let level = |v| if v == 0 { 0 } else { 55 + 40 * v };
            (level(n / 36) << 16) | (level((n / 6) % 6) << 8) | level(n % 6)
        }
        232..=255 => {
            let v = 8 + 10 * (index - 232);
            (v << 16) | (v << 8) | v
        }
        257 => 0x181818,
        _ => 0xe5e5e5,
    }
}

pub fn validate_output<O: RpcMethod>(operation: &O, output: &O::Output) -> Result<(), PeerError> {
    operation
        .validate(output)
        .map_err(|reason| PeerError::InvalidResponse {
            method: operation.method().into(),
            reason: reason.into(),
            sequence: None,
            raw: serde_json::to_string(output).expect("wire output serializes"),
        })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkSession {
    pub thread_id: crate::session::SessionRef,
    pub last_turn_id: crate::ids::TurnId,
}

impl ForkSession {
    pub fn new(thread_id: crate::session::SessionRef, last_turn_id: crate::ids::TurnId) -> Self {
        Self {
            thread_id,
            last_turn_id,
        }
    }
    pub(crate) fn validate(
        &self,
        output: &crate::models::ThreadResponse,
    ) -> Result<(), &'static str> {
        validate_thread(output, None)
    }
}

/// Returns a conversation to just before `turn_id`, which is removed with
/// every turn after it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RewindSession {
    pub thread_id: crate::session::SessionRef,
    pub turn_id: crate::ids::TurnId,
}

impl RewindSession {
    pub fn new(thread_id: crate::session::SessionRef, turn_id: crate::ids::TurnId) -> Self {
        Self { thread_id, turn_id }
    }
    pub(crate) fn validate(
        &self,
        output: &crate::models::ThreadResponse,
    ) -> Result<(), &'static str> {
        validate_thread(output, Some(&self.thread_id))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSession {
    pub provider: crate::session::ProviderKind,
    pub cwd: Option<String>,
    pub model: Option<crate::models::ModelRef>,
    pub initial_input: Option<InitialInput>,
}

/// Creating a task admits its first input before workspace/provider preparation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitialInput {
    pub workspace: Option<crate::workspace::WorkspaceStrategy>,
    pub client_user_message_id: crate::ids::ClientInputId,
    pub input: Vec<Input>,
    pub effort: Option<String>,
    pub service_tier: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryPreparation {
    pub thread_id: crate::session::SessionRef,
    pub work_locally: bool,
}

impl RpcMethod for CreateSession {
    rpc_contract!(CreateSession);
    fn params(&self) -> Result<Self, PeerError> {
        Ok(self.clone())
    }
    fn subscription(output: &mut Self::Output, id: uuid::Uuid) {
        output.subscription_id = id;
    }
    fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
        if output.session.provider != self.provider {
            return Err("created session provider does not match");
        }
        validate_thread(&output.response, Some(&output.session))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Interrupt {
    pub thread_id: crate::session::SessionRef,
    pub turn_id: crate::ids::TurnId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListFiles {
    pub path: String,
}

/// Resolve a filesystem path and inspect its kind on the authenticated Host.
/// File operations accept absolute paths or `~` / `~/...` for the Host user's home.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveFile {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResizeTerminal {
    #[serde(rename = "processHandle")]
    pub handle: String,
    pub size: TerminalSize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetachTerminal {
    #[serde(rename = "processHandle")]
    pub handle: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoveRemoteHost {
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevokeDevice {
    #[serde(rename = "nodeId")]
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadComposerCatalog {
    pub cwd: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadAccountUsage {
    pub provider: crate::session::ProviderKind,
    #[serde(rename = "accountId")]
    pub id: String,
}

impl ListSessions {
    pub fn new(query: crate::models::ListQuery) -> Self {
        Self { query }
    }
}

impl ReadItem {
    pub(crate) fn validate(&self, output: &ItemResponse) -> Result<(), &'static str> {
        if output.item.id == self.item_id {
            Ok(())
        } else {
            Err("item ID does not match")
        }
    }
}

/// Input intent. The Host starts a turn or, while one runs, steers it or queues
/// the input behind it, as `follow_up` asks and the provider allows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Submission {
    /// Start a new native context, preserving the Host's visible transcript.
    /// Such input waits for its own turn even when `follow_up` asks to steer.
    #[serde(default)]
    pub fresh_context: bool,
    pub thread_id: crate::session::SessionRef,
    pub client_user_message_id: crate::ids::ClientInputId,
    pub input: Vec<Input>,
    pub model: Option<crate::models::ModelRef>,
    pub effort: Option<String>,
    #[serde(rename = "serviceTierForTurn")]
    pub service_tier: Option<String>,
    /// Only a running turn reads it, so input that starts one may omit it.
    #[serde(default)]
    pub follow_up: FollowUp,
}

/// What input sent while a turn runs does, as T3 Code's follow-up behavior:
/// wait in the Host's queue until the turn ends, or steer the running turn.
/// A provider that cannot steer queues it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FollowUp {
    #[default]
    Queue,
    Steer,
}

/// Input waiting in the Host's queue, which starts it once the running turn and
/// the input before it have finished. A stopped or failed turn holds the queue
/// until it is resumed, as T3 Code holds queued runs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputQueue {
    pub inputs: Vec<Submission>,
    #[serde(default)]
    pub held: bool,
}

/// A change to a conversation's queue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeQueue {
    pub thread_id: crate::session::SessionRef,
    pub change: QueueChange,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum QueueChange {
    /// Replaces waiting input; it keeps its place in the queue.
    Edit {
        client_user_message_id: crate::ids::ClientInputId,
        input: Vec<Input>,
    },
    Remove {
        client_user_message_id: crate::ids::ClientInputId,
    },
    /// Moves waiting input before `before`, or to the end without it.
    Move {
        client_user_message_id: crate::ids::ClientInputId,
        before: Option<crate::ids::ClientInputId>,
    },
    /// Moves waiting input into the running turn now.
    Steer {
        client_user_message_id: crate::ids::ClientInputId,
    },
    /// Starts a held queue again.
    Resume,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmissionReceipt {
    pub turn_id: Option<crate::ids::TurnId>,
}

#[cfg(test)]
mod usage_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn usage_windows_carry_period_facts_without_a_display_language() {
        for (period, name, expected) in [
            (
                UsagePeriod::Minutes { minutes: 10080 },
                Some("設定".into()),
                json!({
                    "period":{"kind":"minutes","minutes":10080},"name":"設定","remainingPercent":86,"resetsAt":12
                }),
            ),
            (
                UsagePeriod::Monthly,
                None,
                json!({
                    "period":{"kind":"monthly"},"name":null,"remainingPercent":86,"resetsAt":12
                }),
            ),
        ] {
            let value = UsageWindow::from_used(period, name, 13.2, Some(12)).unwrap();
            assert_eq!(serde_json::to_value(&value).unwrap(), expected);
            assert_eq!(
                serde_json::from_value::<UsageWindow>(expected).unwrap(),
                value
            );
        }
        for used in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(UsageWindow::from_used(UsagePeriod::Monthly, None, used, None).is_none());
        }
    }
}
