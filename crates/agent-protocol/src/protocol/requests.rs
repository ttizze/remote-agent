//! One contract per native request; provider JSON conversion stays at its boundary.
use super::*;
use crate::{
    conversation as c, models as m, operations as op, pull_requests as pr,
    scheduled_tasks as st, vcs, workspace as w,
};
macro_rules! contracts {
    ($($variant:ident, $method:literal => ($params:ty, $result:ty) $([$clone:ident])?),* $(,)?) => {
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
        $($(contracts!(@$clone $params, $variant);)?)*
        #[derive(Debug, Clone, Serialize, Deserialize)]
        pub enum Call { $($variant($params)),* }
        impl Call {
            pub fn method(&self) -> &str {
                match self { $(Self::$variant(_) => $method),* }
            }
        }
    };
    (@clone $params:ty, $variant:ident) => {
        impl op::RpcMethod for $params {
            crate::operations::rpc_contract!($variant);
            fn params(&self) -> Result<Self, crate::error::PeerError> { Ok(self.clone()) }
        }
    };
}
contracts! {
    Dispatch, "conversation/dispatch" => (Box<c::Dispatch>, c::Committed) [clone],
    Launch, "conversation/launch" => (Box<c::Launch>, c::Launched) [clone],
    SubscribeThread, "conversation/subscribeThread" => (c::SubscribeThread, c::ThreadUpdate),
    SubscribeShell, "conversation/subscribeShell" => (c::SubscribeShell, c::ShellUpdate),
    GetThread, "conversation/getThread" => (c::GetThread, c::ThreadSnapshot) [clone],
    GetTurnItem, "conversation/getTurnItem" => (c::GetTurnItem, Option<c::TurnItemDetail>) [clone],
    ReadHistory, "conversation/readHistory" => (c::ReadHistory, c::HistoryPage) [clone],
    Search, "conversation/search" => (c::Search, Vec<c::SearchMatch>) [clone],
    GetTurnDiff, "conversation/turnDiff" => (c::GetTurnDiff, c::TurnDiff) [clone],
    ScanAgentSessions, "conversation/agentSessions/scan" => (c::ScanAgentSessions, c::SessionScan) [clone],
    ImportAgentSessions, "conversation/agentSessions/import" => (c::ImportAgentSessions, c::ImportCounts) [clone],
    SetupStream, "conversation/subscribeWorktreeSetup" => (c::SubscribeSetup, Option<agent_domain::WorktreeSetupSnapshot>),
    CancelSetup, "conversation/cancelWorktreeSetup" => (c::CancelSetup, c::SetupCancelled) [clone],
    ListProjects, "host/project/list" => (m::Empty, Vec<m::Project>),
    AddProject, "host/project/add" => (op::AddProject, String) [clone],
    UpdateProject, "host/project/update" => (op::UpdateProject, m::Empty) [clone],
    ProjectFavicon, "host/project/favicon" => (m::ReadProjectFavicon, Option<m::ProjectFavicon>) [clone],
    ReadPermissionSettings, "host/permissions/read" => (crate::permissions::ReadPermissionSettings, crate::permissions::PermissionSettings) [clone],
    UpdatePermissionSettings, "host/permissions/update" => (crate::permissions::UpdatePermissionSettings, crate::permissions::PermissionSettings) [clone],
    ListProviders, "host/provider/list" => (m::Empty, Vec<m::ProviderInstance>),
    ProviderCommands, "host/provider/commands" => (w::ListProviderCommands, w::ProviderCommands) [clone],
    SearchEntries, "host/workspace/searchEntries" => (w::SearchEntries, w::EntrySearch) [clone],
    SearchContents, "host/workspace/searchContents" => (w::SearchContents, w::ContentSearch) [clone],
    VcsStatus, "host/vcs/status" => (w::ReadVcsStatus, w::VcsStatus) [clone],
    ListRefs, "host/vcs/listRefs" => (w::ListRefs, w::RefList) [clone],
    SwitchRef, "host/vcs/switchRef" => (w::SwitchRef, w::SwitchedRef),
    CreateRef, "host/vcs/createRef" => (w::CreateRef, w::SwitchedRef),
    DiffPreview, "host/review/diffPreview" => (w::DiffPreview, w::DiffPreviewResult) [clone],
    Transcribe, "host/dictation/transcribe" => (op::Transcribe, op::Transcription),
    PrepareDictation, "host/dictation/prepare" => (op::DictationPreparation, m::Empty),
    CancelDictation, "host/dictation/cancel" => (op::DictationPreparation, m::Empty),
    ListFiles, "host/file/list" => (op::ListFiles, m::FileList) [clone],
    ReadFile, "host/file/read" => (op::ListFiles, m::FileContent),
    WriteFile, "host/file/write" => (op::WriteFile, m::FileContent) [clone],
    Upload, "host/blob/upload" => (op::Upload, m::TransferGrant),
    AttachmentPath, "host/attachment/path" => (String, String),
    Download, "host/blob/download" => (op::ListFiles, m::TransferGrant),
    ReadVisualization, "host/visualize/read" => (op::LoadVisualization, String) [clone],
    ReviewWorkspace, "host/workspace/review" => (op::ReviewWorkspace, m::WorkspaceReview) [clone],
    ReadWorktreeSettings, "host/worktree/settings/read" => (m::Empty, m::WorktreeSettings),
    UpdateWorktreeSettings, "host/worktree/settings/update" => (m::WorktreeSettings, m::WorktreeSettings),
    ReadSettings, "host/settings/read" => (m::Empty, m::HostSettings),
    Keybindings, "host/keybindings/subscribe" => (m::Empty, crate::keybindings::KeybindingsConfig),
    UpsertKeybinding, "host/keybindings/upsert" => (crate::keybindings::UpsertKeybinding, crate::keybindings::KeybindingsConfig) [clone],
    RemoveKeybinding, "host/keybindings/remove" => (crate::keybindings::KeybindingRule, crate::keybindings::KeybindingsConfig) [clone],
    UpdateSettings, "host/settings/update" => (Box<m::HostSettingsPatch>, m::HostSettings),
    ListWorktrees, "host/worktree/list" => (m::Empty, Vec<m::Worktree>),
    RemoveWorktree, "host/worktree/remove" => (op::RemoveWorktree, ()) [clone],
    ListAccounts, "host/account/list" => (m::Empty, op::Accounts),
    ReadAccountUsage, "host/account/usage" => (op::ReadAccountUsage, op::AccountUsage) [clone],
    ReadUsageSummary, "host/usage/summary" => (op::ReadUsageSummary, crate::usage::Summary) [clone],
    RefreshUsageRates, "host/usage/refreshRates" => (op::RefreshUsageRates, crate::usage::Pricing) [clone],
    ConsumeResetCredit, "host/account/consumeResetCredit" => (op::ConsumeResetCredit, m::Empty) [clone],
    SelectAccount, "host/account/select" => (op::SelectAccount, op::AccountSelection) [clone],
    LogoutAccount, "host/account/logout" => (op::LogoutAccount, m::Empty) [clone],
    StartAccountLogin, "host/account/login/start" => (op::StartAccountLogin, op::AccountLogin) [clone],
    SubmitAccountLogin, "host/account/login/submit" => (op::SubmitAccountLogin, m::Empty) [clone],
    ReadAccountLogin, "host/account/login/status" => (op::ReadAccountLogin, op::AccountLoginStatus) [clone],
    CancelAccountLogin, "host/account/login/cancel" => (op::CancelAccountLogin, m::Empty) [clone],
    StartTerminal, "host/terminal/start" => (op::StartTerminal, m::Empty) [clone],
    ClearTerminal, "host/terminal/clear" => (op::ClearTerminal, m::Empty) [clone],
    RestartTerminal, "host/terminal/restart" => (op::RestartTerminal, m::Empty) [clone],
    TerminalMetadata, "host/terminal/subscribeMetadata" => (m::Empty, op::TerminalMetadataEvent),
    ResizeTerminal, "host/terminal/resize" => (op::ResizeTerminal, m::Empty) [clone],
    WriteTerminal, "host/terminal/write" => (op::TerminalWrite, m::Empty),
    DetachTerminal, "host/terminal/detach" => (op::DetachTerminal, m::Empty) [clone],
    KillTerminal, "host/terminal/kill" => (op::TerminalKill, m::Empty),
    Pair, "host/pair" => (op::Pair, m::Empty) [clone],
    HostName, "host/name" => (m::Empty, String),
    Environment, "host/environment" => (m::Empty, m::EnvironmentDescriptor),
    RegisterAwareness, "host/awareness/register" => (m::AwarenessRegistration, m::AwarenessRegistrationResult) [clone],
    Awareness, "host/awareness/subscribe" => (m::Empty, m::AwarenessSnapshot),
    HostStatus, "host/status" => (m::Empty, m::HostStatus),
    ReadUpdateStatus, "host/update/status" => (m::UpdateStatusRequest, m::UpdateState) [clone],
    CheckUpdate, "host/update/check" => (m::UpdateCheckRequest, m::UpdateState) [clone],
    DownloadUpdate, "host/update/download" => (m::UpdateActionRequest, m::UpdateState) [clone],
    InstallUpdate, "host/update/install" => (m::UpdateActionRequest, m::UpdateState) [clone],
    SetUpdateChannel, "host/update/channel" => (m::UpdateChannelRequest, m::UpdateState) [clone],
    ReadNativeUpdate, "host/update/native" => (m::NativeUpdateRequest, m::NativeUpdateState) [clone],
    Invite, "host/invite" => (m::Empty, m::Invitation),
    ListRemotes, "host/listRemotes" => (m::Empty, Vec<m::RemoteHost>),
    RegisterRemote, "host/registerRemote" => (op::RegisterRemoteHost, m::RemoteHost) [clone],
    RemoveRemote, "host/removeRemote" => (op::RemoveRemoteHost, m::Empty) [clone],
    Revoke, "host/revoke" => (op::RevokeDevice, m::Empty) [clone],
    Browser, "host/browser" => (crate::browser::BrowserRequest, crate::browser::BrowserFrame) [clone],
    PreviewList, "host/preview/list" => (crate::preview::PreviewList, crate::preview::PreviewListResult) [clone],
    PreviewSubscribe, "host/preview/subscribe" => (crate::preview::PreviewSubscribe, crate::preview::PreviewListResult),
    PreviewOpen, "host/preview/open" => (crate::preview::PreviewOpen, crate::preview::PreviewSessionSnapshot) [clone],
    PreviewNavigate, "host/preview/navigate" => (crate::preview::PreviewNavigate, crate::preview::PreviewSessionSnapshot) [clone],
    PreviewResize, "host/preview/resize" => (crate::preview::PreviewResize, crate::preview::PreviewSessionSnapshot) [clone],
    PreviewSetAppearance, "host/preview/appearance" => (crate::preview::PreviewSetAppearance, crate::preview::PreviewSessionSnapshot) [clone],
    PreviewSetZoom, "host/preview/zoom" => (crate::preview::PreviewSetZoom, crate::preview::PreviewSessionSnapshot) [clone],
    PreviewReportStatus, "host/preview/reportStatus" => (crate::preview::PreviewReportStatus, crate::models::Empty) [clone],
    PreviewClose, "host/preview/close" => (crate::preview::PreviewClose, crate::models::Empty) [clone],
    PreviewRefresh, "host/preview/refresh" => (crate::preview::PreviewTab, crate::models::Empty) [clone],
    ConnectionPerformance, "host/diagnostics/connection" => (crate::diagnostics::ConnectionPerformance, m::Empty) [clone],
    ReadBackground, "host/background/read" => (crate::background::ReadBackground, crate::background::BackgroundPolicySnapshot) [clone],
    ReportClientActivity, "host/background/reportActivity" => (crate::background::ReportClientActivity, crate::background::BackgroundPolicySnapshot) [clone],
    ReportHostPowerState, "host/background/reportPower" => (crate::background::HostPowerSnapshot, m::Empty) [clone],
    RemoveClientActivity, "host/background/removeActivity" => (crate::background::RemoveClientActivity, crate::background::BackgroundPolicySnapshot) [clone],
    SubscribeBackground, "host/background/subscribe" => (m::Empty, crate::background::BackgroundPolicySnapshot),
    ReadHostResources, "host/diagnostics/hostResources" => (crate::background::ReadHostResources, crate::background::HostResourcesSnapshot) [clone],
    ReadProcessDiagnostics, "host/diagnostics/processes" => (crate::background::ReadProcessDiagnostics, crate::background::ProcessDiagnosticsResult) [clone],
    ReadProcessResourceHistory, "host/diagnostics/processHistory" => (crate::background::ReadProcessResourceHistory, crate::background::ProcessResourceHistoryResult) [clone],
    ReadTraceDiagnostics, "host/diagnostics/traces" => (crate::background::ReadTraceDiagnostics, crate::background::TraceDiagnosticsResult) [clone],
    // Git operations
    SubscribeVcsStatus, "host/vcs/subscribeStatus" => (vcs::SubscribeVcsStatus, vcs::VcsStatusStreamEvent),
    RefreshVcsStatus, "host/vcs/refreshStatus" => (vcs::RefreshVcsStatus, w::VcsStatus) [clone],
    Pull, "host/vcs/pull" => (vcs::Pull, vcs::PullResult) [clone],
    RunStackedAction, "host/vcs/runStackedAction" => (vcs::RunStackedAction, vcs::ActionProgressEvent),
    InitRepository, "host/vcs/init" => (vcs::InitRepository, m::Empty) [clone],
    CreateWorktree, "host/vcs/createWorktree" => (vcs::CreateWorktree, vcs::CreatedWorktree) [clone],
    RemoveWorktreeCheckout, "host/vcs/removeWorktree" => (vcs::RemoveWorktreeCheckout, m::Empty) [clone],
    ResolvePullRequest, "host/git/resolvePullRequest" => (vcs::ResolvePullRequest, vcs::ResolvedPullRequestResult) [clone],
    PreparePullRequestThread, "host/git/preparePullRequestThread" => (vcs::PreparePullRequestThread, vcs::PreparedPullRequestThread) [clone],
    PublishRepository, "host/sourceControl/publishRepository" => (vcs::PublishRepository, vcs::PublishedRepository) [clone],
    // Scheduled tasks
    ListScheduledTasks, "host/scheduledTasks/list" => (m::Empty, st::ScheduledTaskList),
    SubscribeScheduledTasks, "host/scheduledTasks/subscribe" => (m::Empty, st::ScheduledTaskList),
    UpsertScheduledTask, "host/scheduledTasks/upsert" => (st::UpsertScheduledTask, st::ScheduledTask) [clone],
    SetScheduledTaskEnabled, "host/scheduledTasks/setEnabled" => (st::SetScheduledTaskEnabled, st::ScheduledTask) [clone],
    DeleteScheduledTask, "host/scheduledTasks/delete" => (st::ScheduledTaskRef, st::ScheduledTaskRef),
    RunScheduledTaskNow, "host/scheduledTasks/runNow" => (st::ScheduledTaskRef, st::ScheduledTask),
    ListPullRequests, "host/pullRequests/list" => (pr::ListPullRequests, pr::PullRequestList) [clone],
    GetPullRequest, "host/pullRequests/get" => (pr::GetPullRequest, agent_domain::PullRequestDetail) [clone],
    GetPullRequestDiff, "host/pullRequests/diff" => (pr::GetPullRequestDiff, pr::PullRequestDiff) [clone],
    GetPullRequestDiffFileContents, "host/pullRequests/diffFileContents" => (pr::GetPullRequestDiffFileContents, pr::PullRequestDiffFileContents) [clone],
    GetPullRequestFile, "host/pullRequests/file" => (pr::GetPullRequestFile, pr::PullRequestFile) [clone],
    GetPullRequestViewedFiles, "host/pullRequests/viewedFiles" => (pr::GetPullRequestViewedFiles, pr::PullRequestViewedFiles) [clone],
    SetPullRequestFilesViewed, "host/pullRequests/setViewedFiles" => (pr::SetPullRequestFilesViewed, pr::PullRequestViewedFiles) [clone],
    LinkPullRequest, "host/pullRequests/link" => (pr::LinkPullRequest, pr::PullRequestOperation) [clone],
    UnlinkPullRequest, "host/pullRequests/unlink" => (pr::UnlinkPullRequest, pr::PullRequestOperation) [clone],
    SetPullRequestWatch, "host/pullRequests/watch" => (pr::SetPullRequestWatch, pr::PullRequestOperation) [clone],
    PullRequestAction, "host/pullRequests/action" => (pr::PullRequestActionRequest, pr::PullRequestOperation) [clone],
    SubmitPullRequestReview, "host/pullRequests/review" => (pr::SubmitPullRequestReview, pr::PullRequestOperation) [clone],
    SourceControlAuth, "host/sourceControl/auth" => (pr::SourceControlAuthRequest, pr::SourceControlAuth) [clone],
    SourceControlDiscovery, "host/sourceControl/discovery" => (pr::SourceControlDiscoveryRequest, pr::SourceControlDiscovery) [clone],
    CloneRepository, "host/sourceControl/clone" => (pr::CloneRepository, m::Empty) [clone],
}
