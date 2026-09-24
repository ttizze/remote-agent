//! ABI converters for wire records; the protocol crate has no UniFFI dependency.

use crate::{models::*, session::*};
use agent_protocol::{browser::*, composer::*, diagnostics::*, operations::*};
use serde_json::Value;
use std::collections::BTreeMap;
#[uniffi::remote(Record)]
struct Invitation {
    pub endpoint: String,
    pub invitation: uuid::Uuid,
    pub expires_at: u64,
}
#[uniffi::remote(Record)]
struct Project {
    pub id: String,
    pub name: String,
    pub roots: Vec<ProjectRoot>,
    pub position: Option<u64>,
    pub created_at: Option<u64>,
    pub updated_at: Option<u64>,
}
#[uniffi::remote(Record)]
struct ProjectRoot {
    pub path: String,
}
#[uniffi::remote(Record)]
struct Model {
    pub id: String,
    pub model: String,
    pub display_name: String,
    pub default_reasoning_effort: String,
    pub supported_reasoning_efforts: Vec<ReasoningEffort>,
    pub service_tiers: Option<Vec<ServiceTier>>,
    pub default_service_tier: Option<String>,
    pub is_default: Option<bool>,
}
#[uniffi::remote(Record)]
struct ServiceTier {
    pub id: String,
    pub name: Option<String>,
}
#[uniffi::remote(Record)]
struct ReasoningEffort {
    pub reasoning_effort: String,
}
#[uniffi::remote(Record)]
struct ListQuery {
    pub project_limit: u32,
    pub chat_limit: u32,
    pub project_thread_limits: BTreeMap<String, u32>,
    pub search_term: String,
}
#[uniffi::remote(Record)]
struct FileList {
    pub path: String,
    pub entries: Vec<FileEntry>,
    pub truncated: bool,
}
#[uniffi::remote(Record)]
struct FileEntry {
    pub name: String,
    pub path: String,
    pub directory: bool,
    pub size: u64,
}
#[uniffi::remote(Record)]
struct FileContent {
    pub path: String,
    pub revision: String,
    pub text: String,
    pub bom: bool,
    pub line_ending: String,
    pub size: u64,
}
#[uniffi::remote(Record)]
struct WorktreeSettings {
    pub create_on_new_session: bool,
    pub copy_on_create: bool,
    pub copy_paths: Vec<String>,
    pub worktree_directory: String,
}
#[uniffi::remote(Record)]
struct Worktree {
    pub path: String,
    pub project_path: String,
    pub branch: String,
    pub blocked_reason: Option<String>,
    pub threads: Vec<WorktreeThread>,
}
#[uniffi::remote(Record)]
struct WorktreeThread {
    pub id: String,
    pub name: String,
    pub active: bool,
}
#[uniffi::remote(Enum)]
enum ProviderKind {
    Codex,
    Claude,
}
#[uniffi::remote(Record)]
struct BrowserRequest {
    pub thread_id: String,
    pub control_token: String,
    pub tab_id: String,
    pub image_id: String,
    pub action: BrowserAction,
}
#[uniffi::remote(Enum)]
enum BrowserAction {
    Read,
    TakeControl,
    ReleaseControl,
    Navigate {
        url: String,
    },
    Click {
        x: f64,
        y: f64,
    },
    Scroll {
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
    },
    Type {
        text: String,
    },
    Key {
        key: BrowserKey,
    },
    Back,
    Forward,
    Reload,
    SelectTab {
        id: String,
    },
    Dialog {
        accept: bool,
        text: String,
    },
}
#[uniffi::remote(Enum)]
enum BrowserKey {
    Enter,
    Tab,
    Backspace,
    Escape,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    SelectAll,
}
#[uniffi::remote(Enum)]
enum BrowserControl {
    #[default]
    Agent,
    AwaitingHuman,
    Yours,
    Other,
}
#[uniffi::remote(Record)]
struct BrowserTab {
    pub id: String,
    pub title: String,
    pub url: String,
}
#[uniffi::remote(Record)]
struct BrowserDialog {
    pub message: String,
    pub prompt: bool,
}
#[uniffi::remote(Record)]
struct BrowserFrame {
    pub tabs: Vec<BrowserTab>,
    pub tab_id: String,
    pub control: BrowserControl,
    pub control_token: String,
    pub width: u32,
    pub height: u32,

    pub image: Vec<u8>,
    pub image_id: String,
    pub dialog: Option<BrowserDialog>,
}
#[uniffi::remote(Enum)]
enum InvocationKind {
    Plugin,
    Skill,
}
#[uniffi::remote(Record)]
struct Invocation {
    pub kind: InvocationKind,
    pub name: String,
    pub path: String,
}
#[uniffi::remote(Record)]
struct ComposerCandidate {
    pub invocation: Invocation,
    pub description: String,
}
#[uniffi::remote(Enum)]
enum ConnectionPhase {
    AppPreparation,
    SnapshotRead,
    StoreRestored,
    ClientBuild,
    IdentityRead,
    UiConnectStart,
    UiConnectReady,
    UiConnectFailed,
    UiConnectCancelled,
    ListPublished,
    ListViewUpdated,
    ResumeStart,
    ResumeReady,
    ResumeConnection,
    ResumeFailed,
    ResumeCancelled,
    ResolveFailed,
    QuicFailed,
    NetworkCapture,
    EndpointStart,
    EndpointReady,
    HostDnsStart,
    HostDnsReady,
    HostDnsEnded,
    ResolveStart,
    ResolveReady,
    QuicStart,
    QuicReady,
    EventsOpened,
    AttachStart,
    AttachReady,
    NetworkReportStart,
    NetworkReportReady,
    RelayDialStart,
    RelayRegion,
    RelayDialEnded,
    RelayTcpStart,
    RelayTcpReady,
    RelayTlsStart,
    RelayTlsReady,
    RelayAuthStart,
    RelayAuthReady,
    RelayReady,
    RequestSlotWait,
    RequestOpened,
    RequestEncoded,
    RequestSent,
    ReplyAdopted,
    ReadPolled,
    ReadPending,
    ReadWake,
    ResponseFirstRead,
    ResponseReceived,
    ResponseDecoded,
    RequestFailed,
    PathOpened,
    PathClosed,
    PathSelected,
    PathEventsDropped,
    PathDirect,
    PathRelay,
    PathUnknown,
    RttMicros,
    LostPackets,
    LostBytes,
    CryptoFramesSent,
    CryptoFramesReceived,
    SentPackets,
    ReceivedPackets,
    RuntimePulse,
    AppScene,
}
#[uniffi::remote(Record)]
struct Account {
    pub id: String,
    pub provider: crate::session::ProviderKind,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub usage: Option<AccountUsage>,
}
#[uniffi::remote(Record)]
struct AccountUsage {
    pub windows: Vec<UsageWindow>,
    pub fetched_at: i64,
    pub error: Option<String>,
}
#[uniffi::remote(Record)]
struct UsageWindow {
    pub label: String,
    pub remaining_percent: u32,
    pub resets_at: Option<i64>,
}
#[uniffi::remote(Record)]
struct Accounts {
    pub accounts: Vec<Account>,
    pub selected_id: Option<String>,
    pub selected_claude_id: Option<String>,
    pub error: Option<String>,
}
#[uniffi::remote(Record)]
struct StartAccountLogin {
    pub provider: crate::session::ProviderKind,
}
#[uniffi::remote(Record)]
struct SubmitAccountLogin {
    pub id: String,
    pub code: String,
}
#[uniffi::remote(Record)]
struct AccountLogin {
    pub login_id: String,
    pub requires_code_submission: bool,
    pub user_code: String,
    pub verification_url: String,
}
#[uniffi::remote(Enum)]
enum Answer {
    Decision {
        index: u32,
    },
    Permissions {
        allow: bool,
    },
    Questions {
        answers: std::collections::BTreeMap<String, String>,
    },
    Raw {
        value: Value,
    },
}
#[uniffi::remote(Record)]
struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}
#[uniffi::remote(Record)]
struct AddProject {
    pub cwd: String,
}
#[uniffi::remote(Record)]
struct ListThreads {
    pub query: crate::models::ListQuery,
}
#[uniffi::remote(Record)]
struct ReadItem {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
}
#[uniffi::remote(Record)]
struct OpenRequest {
    pub request_id: Value,
}
#[uniffi::remote(Record)]
struct StartTerminal {
    pub handle: String,
    pub cwd: String,
    pub size: TerminalSize,
}
#[uniffi::remote(Record)]
struct ReviewWorkspace {
    pub cwd: String,
}
#[uniffi::remote(Record)]
struct RemoveWorktree {
    pub path: String,
}
#[uniffi::remote(Record)]
struct SelectAccount {
    pub id: String,
}
#[uniffi::remote(Record)]
struct LogoutAccount {
    pub id: String,
}
#[uniffi::remote(Record)]
struct CancelAccountLogin {
    pub id: String,
}
#[uniffi::remote(Record)]
struct LoadVisualization {
    pub path: String,
    pub cwd: String,
}
#[uniffi::remote(Record)]
struct ForkThread {
    pub thread_id: String,
    pub last_turn_id: String,
    #[uniffi(default = false)]
    pub exclude_turns: bool,
}
#[uniffi::remote(Record)]
struct StartThread {
    pub cwd: Option<String>,
    pub model: Option<String>,
}
#[uniffi::remote(Record)]
struct Interrupt {
    pub thread_id: String,
    pub turn_id: String,
}
#[uniffi::remote(Record)]
struct ListFiles {
    pub path: String,
}
#[uniffi::remote(Record)]
struct ResizeTerminal {
    pub handle: String,
    pub size: TerminalSize,
}
#[uniffi::remote(Record)]
struct DetachTerminal {
    pub handle: String,
}
#[uniffi::remote(Record)]
struct RemoveRemoteHost {
    pub id: String,
}
#[uniffi::remote(Record)]
struct RevokeDevice {
    pub id: String,
}
