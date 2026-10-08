//! ABI converters for retained peripheral wire records.
use crate::{models::*, provider::ProviderKind};
use agent_domain::{Driver, InteractionMode, RuntimeMode};
use agent_protocol::{
    browser::*,
    diagnostics::*,
    operations::*,
    usage::{
        ExternalUsage, PriceOverride, ResetCredits, Resolution, WindowKind as UsageWindowKind,
    },
};
#[uniffi::remote(Enum)]
enum UpdateChannel {
    Nightly,
    Preview,
    Stable,
}
#[uniffi::remote(Enum)]
enum UpdateTarget {
    Host,
    Desktop,
}
#[uniffi::remote(Enum)]
enum NativeUpdatePlatform {
    Android,
    Ios,
}
#[uniffi::remote(Enum)]
enum UpdateStatus {
    Disabled,
    Idle,
    Checking,
    Available,
    Downloading,
    Downloaded,
    Installing,
    UpToDate,
    Error,
}
#[uniffi::remote(Enum)]
enum UpdateErrorContext {
    Check,
    Download,
    Install,
}
#[uniffi::remote(Record)]
struct ReleaseNoteGroup {
    pub title: String,
    pub items: Vec<String>,
}
#[uniffi::remote(Record)]
struct UpdateState {
    pub enabled: bool,
    pub status: UpdateStatus,
    pub target: UpdateTarget,
    pub channel: UpdateChannel,
    pub current_version: String,
    pub available_version: Option<String>,
    pub downloaded_version: Option<String>,
    pub release_notes: Vec<ReleaseNoteGroup>,
    pub omitted_release_count: u32,
    pub download_percent: Option<u8>,
    pub checked_at: Option<String>,
    pub message: Option<String>,
    pub error_context: Option<UpdateErrorContext>,
    pub can_retry: bool,
    pub restart_required: bool,
    pub update_url: Option<String>,
    pub download_url: Option<String>,
    pub artifact_name: Option<String>,
}
#[uniffi::remote(Record)]
struct NativeUpdateState {
    pub platform: NativeUpdatePlatform,
    pub channel: UpdateChannel,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub store_url: Option<String>,
    pub release_notes: Vec<ReleaseNoteGroup>,
    pub checked_at: Option<String>,
    pub message: Option<String>,
}
#[uniffi::remote(Record)]
struct UpdateCheckRequest {
    pub target: UpdateTarget,
    pub current_version: String,
    pub channel: UpdateChannel,
    pub platform: String,
    pub architecture: String,
}
#[uniffi::remote(Record)]
struct UpdateChannelRequest {
    pub target: UpdateTarget,
    pub channel: UpdateChannel,
}
#[uniffi::remote(Record)]
struct NativeUpdateRequest {
    pub platform: NativeUpdatePlatform,
    pub current_version: String,
    pub channel: UpdateChannel,
}
#[uniffi::remote(Enum)]
enum WorktreeStatus {
    Unmerged,
    Merged,
}
#[uniffi::remote(Record)]
struct Invitation {
    pub endpoint: String,
    pub invitation: uuid::Uuid,
    pub expires_at: u64,
    pub host_name: String,
    pub ai_recipients: Vec<String>,
    pub transcription_recipient: Option<String>,
}
#[uniffi::remote(Record)]
struct Project {
    pub id: String,
    pub name: String,
    pub roots: Vec<ProjectRoot>,
    pub scripts: Vec<ProjectScript>,
    pub repository_identity: Option<RepositoryIdentity>,
    pub favicon_path: Option<String>,
    pub created_at: Option<agent_domain::Timestamp>,
    pub updated_at: Option<agent_domain::Timestamp>,
}
#[uniffi::remote(Record)]
struct RepositoryIdentity {
    pub canonical_key: String,
    pub locator: RepositoryLocator,
    pub web_url: Option<String>,
    pub root_path: Option<String>,
    pub display_name: Option<String>,
    pub provider: Option<String>,
    pub owner: Option<String>,
    pub name: Option<String>,
}
#[uniffi::remote(Record)]
struct RepositoryLocator {
    pub source: String,
    pub remote_name: String,
    pub remote_url: String,
}
#[uniffi::remote(Record)]
struct ProjectScript {
    pub id: String,
    pub name: String,
    pub command: String,
    pub icon: ProjectScriptIcon,
    pub run_on_worktree_create: bool,
    pub run_async: Option<bool>,
    pub preview_url: Option<String>,
    pub auto_open_preview: Option<bool>,
}
#[uniffi::remote(Enum)]
enum ProjectScriptIcon {
    Play,
    Test,
    Lint,
    Configure,
    Build,
    Debug,
}
#[uniffi::remote(Record)]
struct ProjectRoot {
    pub path: String,
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
    pub size: u64,
}
#[uniffi::remote(Record)]
struct WorktreeSettings {
    pub create_on_new_session: bool,
    pub copy_on_create: bool,
    pub copy_paths: Vec<String>,
    pub worktree_directory: String,
    pub delete_merged: bool,
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
    pub id: agent_domain::ThreadId,
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
    pub thread_id: agent_domain::ThreadId,
    pub tab_id: String,
    pub image_id: String,
    pub action: BrowserAction,
}
#[uniffi::remote(Enum)]
enum BrowserAction {
    Read,
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
    pub width: u32,
    pub height: u32,

    pub image: Vec<u8>,
    pub image_id: String,
    pub dialog: Option<BrowserDialog>,
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
    pub provider: crate::provider::ProviderKind,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub usage: Option<AccountUsage>,
}
#[uniffi::remote(Record)]
struct AccountUsage {
    pub windows: Vec<UsageWindow>,
    pub fetched_at: i64,
    pub error: Option<String>,
    pub credential_fingerprint: Option<String>,
    pub reset_credits: Option<ResetCredits>,
    pub external_usage: Option<ExternalUsage>,
}
#[uniffi::remote(Record)]
struct UsageWindow {
    pub id: Option<String>,
    pub kind: Option<UsageWindowKind>,
    pub label: String,
    pub used_percent: Option<f64>,
    pub remaining_percent: u32,
    pub window_duration_mins: Option<u32>,
    pub resets_at: Option<i64>,
}
#[uniffi::remote(Enum)]
enum UsageWindowKind {
    Session,
    Weekly,
    Monthly,
    Other,
}
#[uniffi::remote(Record)]
struct ResetCredits {
    pub available_count: u32,
    pub next_expires_at: Option<i64>,
    pub next_credit_id: Option<String>,
}
#[uniffi::remote(Record)]
struct ExternalUsage {
    pub label: String,
    pub url: String,
}
#[uniffi::remote(Enum)]
enum Resolution {
    Day,
    Hour,
}
#[uniffi::remote(Record)]
struct PriceOverride {
    pub input_cost_per_million_tokens: f64,
    pub output_cost_per_million_tokens: f64,
    pub cache_read_cost_per_million_tokens: Option<f64>,
    pub cache_write_cost_per_million_tokens: Option<f64>,
}
#[uniffi::remote(Record)]
struct Accounts {
    pub accounts: Vec<Account>,
    pub selected: std::collections::HashMap<ProviderKind, String>,
    pub error: Option<String>,
}
#[uniffi::remote(Record)]
struct AccountLogin {
    pub provider: ProviderKind,
    pub login_id: String,
    pub requires_code_submission: bool,
    pub user_code: String,
    pub verification_url: String,
}
#[uniffi::remote(Record)]
struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}
#[uniffi::remote(Enum)]
enum Driver {
    Codex,
    Claude,
}
#[uniffi::remote(Enum)]
enum RuntimeMode {
    ApprovalRequired,
    AutoAcceptEdits,
    Auto,
    FullAccess,
}
#[uniffi::remote(Enum)]
enum InteractionMode {
    Default,
    Plan,
}
