//! One contract per native request; provider JSON conversion stays at its boundary.
use super::*;
use crate::{conversation as c, models as m, operations as op, workspace as w};
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
    UpdateProvider, "host/provider/update" => (op::UpdateProvider, op::ProviderUpdate) [clone],
    SearchAcpRegistry, "host/provider/acp/search" => (op::SearchAcpRegistry, op::AcpRegistrySearchResult) [clone],
    PrepareAcpAgent, "host/provider/acp/prepare" => (op::PrepareAcpAgent, op::PreparedAcpAgent) [clone],
    UninstallAcpAgent, "host/provider/acp/uninstall" => (op::UninstallAcpAgent, op::UninstalledAcpAgent) [clone],
    ProbeAcpAgent, "host/provider/acp/probe" => (op::ProbeAcpAgent, op::AcpProbeResult) [clone],
    SearchEntries, "host/workspace/searchEntries" => (w::SearchEntries, w::EntrySearch) [clone],
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
    HostStatus, "host/status" => (m::Empty, m::HostStatus),
    ReadHostResources, "host/resources/read" => (m::Empty, m::HostResourcesSnapshot),
    Invite, "host/invite" => (m::Empty, m::Invitation),
    ListRemotes, "host/listRemotes" => (m::Empty, Vec<m::RemoteHost>),
    RegisterRemote, "host/registerRemote" => (op::RegisterRemoteHost, m::RemoteHost) [clone],
    RemoveRemote, "host/removeRemote" => (op::RemoveRemoteHost, m::Empty) [clone],
    Revoke, "host/revoke" => (op::RevokeDevice, m::Empty) [clone],
    Browser, "host/browser" => (crate::browser::BrowserRequest, crate::browser::BrowserFrame) [clone],
    ConnectionPerformance, "host/diagnostics/connection" => (crate::diagnostics::ConnectionPerformance, m::Empty) [clone],
}
