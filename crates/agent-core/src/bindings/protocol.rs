//! ABI converters for wire records; the protocol crate has no UniFFI dependency.

use crate::{models::*, session::*};
use agent_protocol::editors::*;
use agent_protocol::execution::{BackgroundOutcome, BackgroundReport, BackgroundTaskKind};
use agent_protocol::file_metadata::{FileMetadata, FileMetadataKind, FilePathMetadata};
use agent_protocol::file_tree::EntryKind;
use agent_protocol::live_activity::*;
use agent_protocol::permissions::*;
use agent_protocol::project_import::ScanProjects;
use agent_protocol::providers::*;
use agent_protocol::{browser::*, composer::*, diagnostics::*, operations::*, requests::*};
use serde_json::Value;
#[uniffi::remote(Record)]
struct ReadProviderSettings {}
#[uniffi::remote(Record)]
struct ReadHostSoftware {}
#[uniffi::remote(Record)]
struct HostSoftware {
    pub version: String,
    pub revision: String,
    pub build: u64,
}
#[uniffi::remote(Record)]
struct SetProviderEnabled {
    pub provider: ProviderKind,
    pub enabled: bool,
}
#[uniffi::remote(Record)]
struct CodexCliStatus {
    pub running: bool,
    pub version: Option<String>,
    pub restart_required: bool,
    pub error: Option<String>,
}
#[uniffi::remote(Enum)]
enum FollowUp {
    Queue,
    Steer,
}
#[uniffi::remote(Enum)]
enum BackgroundOutcome {
    Completed,
    Failed,
    Cancelled,
    Updated,
    Unknown,
}
#[uniffi::remote(Record)]
struct BackgroundReport {
    pub kind: BackgroundTaskKind,
    pub label: Option<String>,
    pub outcome: BackgroundOutcome,
    pub exit_code: Option<i32>,
}
#[uniffi::remote(Enum)]
enum BackgroundTaskKind {
    Subagent,
    Command,
    Monitor,
    BackgroundTask,
}
#[uniffi::remote(Enum)]
enum EntryKind {
    Directory,
    File,
}
#[uniffi::remote(Enum)]
enum FileMetadataKind {
    File,
    Directory,
    Other,
}
#[uniffi::remote(Record)]
struct FileMetadata {
    pub kind: FileMetadataKind,
    pub byte_length: Option<u64>,
    pub mime_type: Option<String>,
}
#[uniffi::remote(Record)]
struct FilePathMetadata {
    pub path: String,
    pub metadata: Option<FileMetadata>,
}
#[uniffi::remote(Enum)]
enum EditorId {
    Cursor,
    Trae,
    Kiro,
    VsCode,
    VsCodeInsiders,
    VsCodium,
    Zed,
    Antigravity,
    Idea,
    Aqua,
    Clion,
    Datagrip,
    Dataspell,
    Goland,
    Phpstorm,
    Pycharm,
    Rider,
    Rubymine,
    Rustrover,
    Webstorm,
    FileManager,
}
#[uniffi::remote(Enum)]
enum FileManagerRevealKind {
    Finder,
    FileExplorer,
    Files,
}
#[uniffi::remote(Record)]
struct FileOpenCapabilities {
    pub editors: Vec<EditorId>,
    pub reveal: Option<FileManagerRevealKind>,
}
#[uniffi::remote(Record)]
struct ReadFileOpenCapabilities {}
#[uniffi::remote(Record)]
struct OpenInEditor {
    pub path: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub editor: EditorId,
    pub reveal: bool,
}
#[uniffi::remote(Enum)]
enum ListDecorationScope {
    Root,
    Project { project_id: String },
}
#[uniffi::remote(Enum)]
enum PushEnvironment {
    Sandbox,
    Production,
}
#[uniffi::remote(Record)]
struct RegisterLiveActivity {
    pub activity_id: Option<String>,
    pub allow_start: bool,
    pub token: Vec<u8>,
    pub environment: PushEnvironment,
}
#[uniffi::remote(Enum)]
enum TaskActivityIconKind {
    Running,
    Waiting,
    Unknown,
    Finished,
}
#[uniffi::remote(Record)]
struct TaskActivityIcon {
    pub kind: TaskActivityIconKind,
    pub label: String,
}
#[uniffi::remote(Record)]
struct TaskActivityDisplay {
    pub current: TaskActivityView,
    pub can_start: bool,
    pub ongoing: bool,
    pub urgent: bool,
}
#[uniffi::remote(Record)]
struct TaskActivityView {
    pub total: u32,
    pub label: String,
    pub icons: Vec<TaskActivityIcon>,
    pub overflow: u32,
}
#[uniffi::remote(Record)]
struct UnregisterLiveActivity {
    pub activity_id: String,
}
#[uniffi::remote(Record)]
struct CreateInvitation {}
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
struct ProjectRoot {
    pub path: String,
}
#[uniffi::remote(Record)]
struct ModelRef {
    pub provider: ProviderKind,
    pub id: String,
}
#[uniffi::remote(Record)]
struct Model {
    pub id: String,
    pub model: ModelRef,
    pub display_name: String,
    pub credential_route: Option<agent_protocol::dsh::KeyProvider>,
    pub settings: Option<ModelSettings>,
    pub settings_discovery: bool,
    pub is_default: Option<bool>,
}
#[uniffi::remote(Record)]
struct ModelSettings {
    pub default_reasoning_effort: String,
    pub supported_reasoning_efforts: Vec<ReasoningEffort>,
    pub service_tiers: Option<Vec<ServiceTier>>,
    pub default_service_tier: Option<String>,
}
#[uniffi::remote(Record)]
struct ReadModelSettings {
    pub model: ModelRef,
}
#[uniffi::remote(Record)]
struct ServiceTier {
    pub id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub fast: bool,
}
#[uniffi::remote(Record)]
struct ReasoningEffort {
    pub reasoning_effort: String,
}
#[uniffi::remote(Record)]
struct ListQuery {
    pub limit: u32,
    pub project_limit: u32,
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
    pub size: u64,
    pub truncated: bool,
}
#[uniffi::remote(Record)]
struct WorktreeSettings {
    pub create_on_new_session: bool,
    pub copy_on_create: bool,
    pub copy_paths: Vec<String>,
    pub worktree_directory: String,
    pub delete_unchanged: bool,
    pub project_delete_unchanged: std::collections::HashMap<String, bool>,
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
    pub id: SessionRef,
    pub name: String,
    pub active: bool,
}
#[uniffi::remote(Enum)]
enum ProviderKind {
    Codex,
    Claude,
    DeepSeek,
}
#[uniffi::remote(Record)]
struct SessionRef {
    pub provider: Option<ProviderKind>,
    pub id: String,
}
#[uniffi::remote(Record)]
struct BrowserRequest {
    pub thread_id: SessionRef,
    pub tab_id: String,
    pub image_id: String,
    pub generation: u64,
    pub action: BrowserAction,
    pub renderer_id: String,
}
#[uniffi::remote(Enum)]
enum BrowserAction {
    Read,
    Credentials,
    ImportCookies {
        profile: String,
    },
    ImportPasswords {
        profile: String,
    },
    FillPassword {
        origin: String,
        username: String,
    },
    DeletePasswords {
        origin: String,
    },
    OpenTab {
        url: String,
    },
    CloseTab {
        id: String,
    },
    Resize {
        width: u32,
        height: u32,
    },
    Navigate {
        url: String,
    },
    Click {
        x: f64,
        y: f64,
    },
    Hover {
        x: f64,
        y: f64,
    },
    Drag {
        x: f64,
        y: f64,
        to_x: f64,
        to_y: f64,
    },
    Snapshot,
    Evaluate {
        expression: String,
    },
    ClickElement {
        selector: String,
    },
    Fill {
        selector: String,
        text: String,
    },
    SelectOption {
        selector: String,
        value: String,
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
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
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
struct BrowserImportProfile {
    pub id: String,
    pub label: String,
}
#[uniffi::remote(Record)]
struct BrowserImportError {
    pub browser: String,
    pub message: String,
}
#[uniffi::remote(Record)]
struct BrowserCredentials {
    pub profiles: Vec<BrowserImportProfile>,
    pub import_errors: Vec<BrowserImportError>,
    pub origin: Option<String>,
    pub accounts: Vec<String>,
    pub password_error: Option<String>,
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
    pub input_kind: Option<BrowserInputKind>,
    pub generation: u64,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub loading: bool,
    pub page_text: String,
    pub value: String,
    pub notice: Option<BrowserNotice>,
    pub credentials: Option<BrowserCredentials>,
    pub renderer_id: String,
}
#[uniffi::remote(Enum)]
enum BrowserNotice {
    CookiesImported { count: u64, skipped: u64 },
    PasswordsImported { count: u64, skipped: u64 },
    PasswordsDeleted,
    PasswordFilled,
}
#[uniffi::remote(Enum)]
enum BrowserInputKind {
    Text,
    Password,
}
#[uniffi::remote(Enum)]
enum InvocationKind {
    Plugin,
    Skill,
}
#[uniffi::remote(Record)]
struct Invocation {
    pub provider: ProviderKind,
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
#[uniffi::remote(Enum)]
enum UsagePeriod {
    Minutes { minutes: u64 },
    Monthly,
}
#[uniffi::remote(Record)]
struct UsageWindow {
    pub period: UsagePeriod,
    pub name: Option<String>,
    pub remaining_percent: u32,
    pub resets_at: Option<i64>,
}
#[uniffi::remote(Record)]
struct Accounts {
    pub accounts: Vec<Account>,
    pub selected: std::collections::HashMap<ProviderKind, String>,
    pub errors: std::collections::HashMap<ProviderKind, String>,
}
#[uniffi::remote(Record)]
struct SubmitAccountLogin {
    pub provider: ProviderKind,
    pub id: String,
    pub code: String,
}
#[uniffi::remote(Record)]
struct AccountLogin {
    pub provider: ProviderKind,
    pub login_id: String,
    pub requires_code_submission: bool,
    pub user_code: String,
    pub verification_url: String,
}
#[uniffi::remote(Enum)]
enum RequestBody {
    Approval {
        kind: ApprovalKind,
        description: String,
        details: String,
        choices: Vec<Choice>,
    },
    Permission {
        description: String,
        details: String,
        choices: Vec<Choice>,
    },
    Question {
        questions: Vec<Question>,
    },
    Elicitation {
        server: String,
        message: String,
        input: ElicitationInput,
    },
    ToolExecution {
        tool: String,
        namespace: Option<String>,
        arguments: Value,
    },
}

#[uniffi::remote(Enum)]
enum ApprovalKind {
    Command,
    FileChange,
    Tool,
}

#[uniffi::remote(Record)]
struct Choice {
    pub id: String,
    pub label: String,
    pub description: String,
    pub effect: ChoiceEffect,
}

#[uniffi::remote(Enum)]
enum ChoiceEffect {
    Allow,
    AllowLasting,
    Deny,
}

#[uniffi::remote(Record)]
struct Question {
    pub id: String,
    pub header: String,
    pub prompt: String,
    pub secret: bool,
    pub allow_free_text: bool,
    pub multiple: bool,
    pub choices: Vec<QuestionChoice>,
}

#[uniffi::remote(Record)]
struct QuestionChoice {
    pub id: String,
    pub label: String,
    pub description: String,
}

#[uniffi::remote(Enum)]
enum ElicitationInput {
    Form { fields: Vec<FormField> },
    Url { url: String },
}

#[uniffi::remote(Record)]
struct FormField {
    pub name: String,
    pub title: String,
    pub description: String,
    pub required: bool,
    pub input: FormInput,
}

#[uniffi::remote(Enum)]
enum FormInput {
    String {
        min_length: Option<u64>,
        max_length: Option<u64>,
        format: Option<StringFormat>,
        default: Option<String>,
    },
    Number {
        integer: bool,
        minimum: Option<f64>,
        maximum: Option<f64>,
        default: Option<f64>,
    },
    Boolean {
        default: Option<bool>,
    },
    Choice {
        choices: Vec<FormChoice>,
        default: Option<String>,
    },
    Multiple {
        choices: Vec<FormChoice>,
        min_items: Option<u64>,
        max_items: Option<u64>,
        default: Vec<String>,
    },
}

#[uniffi::remote(Enum)]
enum StringFormat {
    Email,
    Uri,
    Date,
    DateTime,
}

#[uniffi::remote(Record)]
struct FormChoice {
    pub value: String,
    pub title: String,
}

#[uniffi::remote(Enum)]
enum Answer {
    Approval {
        choice_id: String,
    },
    Permission {
        choice_id: String,
    },
    Questions {
        answers: std::collections::BTreeMap<String, QuestionAnswer>,
    },
    SkipQuestions,
    Elicitation {
        action: ElicitationAnswer,
    },
    ToolExecution {
        success: bool,
        content: Vec<ToolContent>,
    },
}

#[uniffi::remote(Enum)]
enum QuestionAnswer {
    FreeText { text: String },
    SingleChoice { choice_id: String },
    MultipleChoices { choice_ids: Vec<String> },
}

#[uniffi::remote(Enum)]
enum ElicitationAnswer {
    Accept { values: Value },
    Decline,
    Cancel,
}

#[uniffi::remote(Enum)]
enum ToolContent {
    Text { text: String },
    Image { data_url: String },
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
struct ScanProjects {}
#[uniffi::remote(Record)]
struct ListSessions {
    pub query: crate::models::ListQuery,
}
#[uniffi::remote(Record)]
struct ListProjectSessions {
    pub project_id: String,
    pub limit: u32,
    pub search_term: String,
}
#[uniffi::remote(Record)]
struct ListAgents {
    pub thread_id: SessionRef,
}
#[uniffi::remote(Record)]
struct ReadItem {
    pub thread_id: SessionRef,
    pub turn_id: agent_protocol::ids::TurnId,
    pub item_id: agent_protocol::ids::ItemId,
}
#[uniffi::remote(Record)]
struct OpenRequest {
    pub request_id: agent_protocol::ids::RequestId,
}
#[uniffi::remote(Record)]
struct StartTerminal {
    pub handle: String,
    pub cwd: String,
    pub size: TerminalSize,
}
#[uniffi::remote(Record)]
struct StartAccountTerminal {
    pub handle: String,
    pub provider: ProviderKind,
    pub id: String,
    pub size: TerminalSize,
}
#[uniffi::remote(Record)]
struct ReviewWorkspace {
    pub cwd: String,
}
#[uniffi::remote(Record)]
struct WriteWorkspaceFile {
    pub cwd: String,
    pub relative_path: String,
    pub text: String,
}
#[uniffi::remote(Record)]
struct RemoveWorktree {
    pub path: String,
}
#[uniffi::remote(Record)]
struct SelectAccount {
    pub provider: ProviderKind,
    pub id: String,
}
#[uniffi::remote(Record)]
struct LogoutAccount {
    pub provider: ProviderKind,
    pub id: String,
}
#[uniffi::remote(Record)]
struct CancelAccountLogin {
    pub provider: ProviderKind,
    pub id: String,
}
#[uniffi::remote(Record)]
struct LoadVisualization {
    pub path: String,
    pub cwd: String,
}
#[uniffi::remote(Record)]
struct ForkSession {
    pub thread_id: SessionRef,
    pub last_turn_id: agent_protocol::ids::TurnId,
}
#[uniffi::remote(Record)]
struct RewindSession {
    pub thread_id: SessionRef,
    pub turn_id: agent_protocol::ids::TurnId,
}
#[uniffi::remote(Record)]
struct InitialInput {
    pub client_user_message_id: agent_protocol::ids::ClientInputId,
    pub input: Vec<Input>,
    pub effort: Option<String>,
    pub service_tier: Option<String>,
}
#[uniffi::remote(Enum)]
enum Input {
    Text { text: String },
    Skill { name: String, path: String },
    LocalImage { path: String },
    Mention { path: String, name: String },
}
#[uniffi::remote(Record)]
struct RetryPreparation {
    pub input_id: agent_protocol::ids::ClientInputId,
    pub thread_id: SessionRef,
}
#[uniffi::remote(Record)]
struct Interrupt {
    pub thread_id: SessionRef,
    pub turn_id: agent_protocol::ids::TurnId,
}
#[uniffi::remote(Record)]
struct ListFiles {
    pub path: String,
}

#[uniffi::remote(Record)]
struct ResolveFile {
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

#[uniffi::remote(Enum)]
enum PermissionMode {
    Ask,
    Auto,
    FullAccess,
}
#[uniffi::remote(Record)]
struct ReadPermissionSettings {
    pub provider: ProviderKind,
}
#[uniffi::remote(Record)]
struct UpdatePermissionSettings {
    pub provider: ProviderKind,
    pub mode: PermissionMode,
    pub version: String,
}

use agent_protocol::dsh::*;
#[uniffi::remote(Enum)]
enum KeyProvider {
    DeepSeek,
    OpenRouter,
    NanoGpt,
    OpenCode,
    OpenAI,
    Anthropic,
    Gemini,
    Groq,
    Mistral,
    Xai,
}
#[uniffi::remote(Record)]
struct ReadCredentials {}
#[uniffi::remote(Record)]
struct KeyUpdate {
    pub provider: KeyProvider,
    pub key: Option<String>,
}
