//! Requests outside the conversation stream: models, files, accounts,
//! terminals, transfers and pairing.
use super::{
    Outcome, Peer, invalid,
    owner::{Event, FileTransfer, Owner, Waiter},
};
use crate::{peer::PeerError, protocol::Call, state::*};
use agent_protocol::{
    background as bg, conversation as c, device as d, models as m, operations as op,
    pull_requests as pr, scheduled_tasks as st, workspace as w,
};
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub(super) struct JobResult {
    pub call: Call,
    pub result: Result<Reply, PeerError>,
    pub complete: Option<Waiter>,
    pub sent: Option<(String, Draft)>,
    pub diff_generation: Option<u64>,
}

pub(super) enum Reply {
    Search(Vec<c::SearchMatch>),
    Providers(Vec<m::ProviderInstance>),
    ProjectAdded(String),
    Files(m::FileList),
    File(m::FileContent),
    Review(m::WorkspaceReview),
    TurnDiff(c::TurnDiff),
    WorktreeSettings(m::WorktreeSettings),
    Worktrees(Vec<m::Worktree>),
    Accounts(op::Accounts),
    UsageSummary(agent_protocol::usage::Summary),
    UsagePricing(agent_protocol::usage::Pricing),
    Login(op::AccountLogin),
    HostStatus(m::HostStatus),
    UpdateStatus(m::UpdateState),
    NativeUpdate(m::NativeUpdateState),
    Remotes(Vec<m::RemoteHost>),
    Remote(m::RemoteHost),
    Invitation(m::Invitation),
    PreviewList(agent_protocol::preview::PreviewListResult),
    PreviewSession(agent_protocol::preview::PreviewSessionSnapshot),
    PreviewRecordingStatus(agent_protocol::preview::PreviewRecordingStatus),
    PreviewRecordingArtifact(agent_protocol::preview::PreviewRecordingArtifact),
    ContentSearch(agent_protocol::workspace::ContentSearch),
    Environment(m::EnvironmentDescriptor),
    AwarenessRegistration(m::AwarenessRegistrationResult),
    Transcription(String),
    HostSettings(m::HostSettings),
    SessionScan(c::SessionScan),
    ProviderCommands(w::ProviderCommands),
    ProviderUpdate(op::ProviderUpdate),
    AcpRegistrySearch(op::AcpRegistrySearchResult),
    PreparedAcpAgent(op::PreparedAcpAgent),
    UninstalledAcpAgent(op::UninstalledAcpAgent),
    AcpProbe(op::AcpProbeResult),
    EntrySearch(w::EntrySearch),
    VcsStatus(w::VcsStatus),
    Refs(w::RefList),
    DiffPreview(w::DiffPreviewResult),
    PullResult(agent_protocol::vcs::PullResult),
    CreatedWorktree(agent_protocol::vcs::CreatedWorktree),
    ResolvedPullRequest(agent_protocol::vcs::ResolvedPullRequestResult),
    PreparedPullRequestThread(agent_protocol::vcs::PreparedPullRequestThread),
    PublishedRepository(agent_protocol::vcs::PublishedRepository),
    PullRequestList(pr::PullRequestList),
    PullRequestDetail(agent_domain::PullRequestDetail),
    PullRequestDiff(pr::PullRequestDiff),
    PullRequestDiffFileContents(pr::PullRequestDiffFileContents),
    PullRequestFile(pr::PullRequestFile),
    PullRequestViewedFiles(pr::PullRequestViewedFiles),
    PullRequestOperation(pr::PullRequestOperation),
    PullRequestAuth(pr::SourceControlAuth),
    PullRequestDiscovery(pr::SourceControlDiscovery),
    DeviceState(d::DeviceServiceState),
    DeviceSession(d::DeviceSession),
    DeviceDetail(d::DeviceDetail),
    DeviceScreenshot(d::DeviceScreenshot),
    DeviceAccessibility(d::DeviceAccessibilityTree),
    DeviceEventLog(Vec<d::DeviceEventLogEntry>),
    DeviceRecordingStatus(d::DeviceRecordingStatus),
    DeviceRecording(d::DeviceRecording),
    SetupCancelled(c::SetupCancelled),
    ProjectIcon(Option<m::ProjectFavicon>),
    SwitchedRef(w::SwitchedRef),
    Keybindings(agent_protocol::keybindings::KeybindingsConfig),
    ScheduledTasks(st::ScheduledTaskList),
    ScheduledTask(st::ScheduledTask),
    ScheduledTaskRef(st::ScheduledTaskRef),
    Background(bg::BackgroundPolicySnapshot),
    HostResources(bg::HostResourcesSnapshot),
    ProcessDiagnostics(bg::ProcessDiagnosticsResult),
    ProcessResourceHistory(bg::ProcessResourceHistoryResult),
    TraceDiagnostics(bg::TraceDiagnosticsResult),
    Done,
}

async fn execute(peer: &Peer, call: &Call) -> Result<Reply, PeerError> {
    Ok(match call {
        Call::Search(_) => Reply::Search(peer.request(call).await?),
        Call::AddProject(_) => Reply::ProjectAdded(peer.request(call).await?),
        Call::ListProviders(_) => Reply::Providers(peer.request(call).await?),
        Call::ListFiles(_) => Reply::Files(peer.request(call).await?),
        Call::ReadFile(_) | Call::WriteFile(_) => Reply::File(peer.request(call).await?),
        Call::ReviewWorkspace(_) => Reply::Review(peer.request(call).await?),
        Call::GetTurnDiff(_) => Reply::TurnDiff(peer.request(call).await?),
        Call::ReadWorktreeSettings(_) | Call::UpdateWorktreeSettings(_) => {
            Reply::WorktreeSettings(peer.request(call).await?)
        }
        Call::ListWorktrees(_) => Reply::Worktrees(peer.request(call).await?),
        Call::ReadSettings(_) | Call::UpdateSettings(_) => {
            Reply::HostSettings(peer.request(call).await?)
        }
        Call::ScanAgentSessions(_) => Reply::SessionScan(peer.request(call).await?),
        Call::ProviderCommands(_) => Reply::ProviderCommands(peer.request(call).await?),
        Call::UpdateProvider(_) => Reply::ProviderUpdate(peer.request(call).await?),
        Call::SearchAcpRegistry(_) => Reply::AcpRegistrySearch(peer.request(call).await?),
        Call::PrepareAcpAgent(_) => Reply::PreparedAcpAgent(peer.request(call).await?),
        Call::UninstallAcpAgent(_) => Reply::UninstalledAcpAgent(peer.request(call).await?),
        Call::ProbeAcpAgent(_) => Reply::AcpProbe(peer.request(call).await?),
        Call::SearchEntries(_) => Reply::EntrySearch(peer.request(call).await?),
        Call::VcsStatus(_) => Reply::VcsStatus(peer.request(call).await?),
        Call::RefreshVcsStatus(_) => Reply::VcsStatus(peer.request(call).await?),
        Call::Pull(_) => Reply::PullResult(peer.request(call).await?),
        Call::InitRepository(_) | Call::RemoveWorktreeCheckout(_) => {
            let _: m::Empty = peer.request(call).await?;
            Reply::Done
        }
        Call::CreateWorktree(_) => Reply::CreatedWorktree(peer.request(call).await?),
        Call::ResolvePullRequest(_) => Reply::ResolvedPullRequest(peer.request(call).await?),
        Call::PreparePullRequestThread(_) => {
            Reply::PreparedPullRequestThread(peer.request(call).await?)
        }
        Call::PublishRepository(_) => Reply::PublishedRepository(peer.request(call).await?),
        Call::RunStackedAction(_) | Call::SubscribeVcsStatus(_) => Reply::Done,
        Call::ListRefs(_) => Reply::Refs(peer.request(call).await?),
        Call::DiffPreview(_) => Reply::DiffPreview(peer.request(call).await?),
        Call::ListPullRequests(_) => Reply::PullRequestList(peer.request(call).await?),
        Call::GetPullRequest(_) => Reply::PullRequestDetail(peer.request(call).await?),
        Call::GetPullRequestDiff(_) => Reply::PullRequestDiff(peer.request(call).await?),
        Call::GetPullRequestDiffFileContents(_) => {
            Reply::PullRequestDiffFileContents(peer.request(call).await?)
        }
        Call::GetPullRequestFile(_) => Reply::PullRequestFile(peer.request(call).await?),
        Call::GetPullRequestViewedFiles(_) | Call::SetPullRequestFilesViewed(_) => {
            Reply::PullRequestViewedFiles(peer.request(call).await?)
        }
        Call::LinkPullRequest(_)
        | Call::UnlinkPullRequest(_)
        | Call::SetPullRequestWatch(_)
        | Call::PullRequestAction(_)
        | Call::SubmitPullRequestReview(_) => Reply::PullRequestOperation(peer.request(call).await?),
        Call::SourceControlAuth(_) => Reply::PullRequestAuth(peer.request(call).await?),
        Call::SourceControlDiscovery(_) => Reply::PullRequestDiscovery(peer.request(call).await?),
        Call::DeviceList(_) | Call::DeviceConfigure(_) | Call::DeviceHosts(_) => {
            Reply::DeviceState(peer.request(call).await?)
        }
        Call::DeviceOpen(_) => Reply::DeviceSession(peer.request(call).await?),
        Call::DeviceDetail(_) | Call::DeviceAction(_) => {
            Reply::DeviceDetail(peer.request(call).await?)
        }
        Call::DeviceScreenshot(_) => Reply::DeviceScreenshot(peer.request(call).await?),
        Call::DeviceInput(_) => { let _: m::Empty = peer.request(call).await?; Reply::Done }
        Call::DeviceAccessibility(_) => Reply::DeviceAccessibility(peer.request(call).await?),
        Call::DeviceEventLog(_) => Reply::DeviceEventLog(peer.request(call).await?),
        Call::DeviceRecordingStart(_) => Reply::DeviceRecordingStatus(peer.request(call).await?),
        Call::DeviceRecordingStop(_) => Reply::DeviceRecording(peer.request(call).await?),
        Call::DeviceClose(_) | Call::DeviceShutdown(_) => {
            let _: m::Empty = peer.request(call).await?;
            Reply::Done
        }
        Call::DeviceSubscribe(_) => unreachable!("device subscriptions use the stream owner"),
        Call::ProjectFavicon(_) => Reply::ProjectIcon(peer.request(call).await?),
        Call::SwitchRef(_) | Call::CreateRef(_) => Reply::SwitchedRef(peer.request(call).await?),
        Call::UpsertKeybinding(_) | Call::RemoveKeybinding(_) => {
            Reply::Keybindings(peer.request(call).await?)
        }
        Call::ListAccounts(_) => Reply::Accounts(peer.request(call).await?),
        Call::ReadUsageSummary(_) => Reply::UsageSummary(peer.request(call).await?),
        Call::RefreshUsageRates(_) => Reply::UsagePricing(peer.request(call).await?),
        Call::StartAccountLogin(_) => Reply::Login(peer.request(call).await?),
        Call::HostStatus(_) => Reply::HostStatus(peer.request(call).await?),
        Call::ReadUpdateStatus(_)
        | Call::CheckUpdate(_)
        | Call::DownloadUpdate(_)
        | Call::InstallUpdate(_)
        | Call::SetUpdateChannel(_) => Reply::UpdateStatus(peer.request(call).await?),
        Call::ReadNativeUpdate(_) => Reply::NativeUpdate(peer.request(call).await?),
        Call::ListRemotes(_) => Reply::Remotes(peer.request(call).await?),
        Call::RegisterRemote(_) => Reply::Remote(peer.request(call).await?),
        Call::Invite(_) => Reply::Invitation(peer.request(call).await?),
        Call::PreviewList(_) => Reply::PreviewList(peer.request(call).await?),
        Call::PreviewOpen(_)
        | Call::PreviewNavigate(_)
        | Call::PreviewResize(_)
        | Call::PreviewSetAppearance(_)
        | Call::PreviewSetZoom(_) => Reply::PreviewSession(peer.request(call).await?),
        Call::PreviewRecordingStart(_) => {
            Reply::PreviewRecordingStatus(peer.request(call).await?)
        }
        Call::PreviewRecordingStop(_) => {
            Reply::PreviewRecordingArtifact(peer.request(call).await?)
        }
        Call::PreviewReportStatus(_) | Call::PreviewClose(_) | Call::PreviewRefresh(_) => {
            let _: m::Empty = peer.request(call).await?;
            Reply::Done
        }
        Call::SearchContents(_) => Reply::ContentSearch(peer.request(call).await?),
        Call::Environment(_) => Reply::Environment(peer.request(call).await?),
        Call::RegisterAwareness(_) => Reply::AwarenessRegistration(peer.request(call).await?),
        Call::Transcribe(_) => {
            Reply::Transcription(peer.request::<op::Transcription>(call).await?.text)
        }
        Call::SelectAccount(_) => {
            let _: op::AccountSelection = peer.request(call).await?;
            Reply::Done
        }
        Call::RemoveWorktree(_) => {
            let _: () = peer.request(call).await?;
            Reply::Done
        }
        Call::CancelSetup(_) => Reply::SetupCancelled(peer.request(call).await?),
        Call::UpsertScheduledTask(_)
        | Call::SetScheduledTaskEnabled(_)
        | Call::RunScheduledTaskNow(_) => {
            Reply::ScheduledTask(peer.request(call).await?)
        }
        Call::ListScheduledTasks(_) => Reply::ScheduledTasks(peer.request(call).await?),
        Call::DeleteScheduledTask(_) => Reply::ScheduledTaskRef(peer.request(call).await?),
        Call::ReadBackground(_)
        | Call::UpdateBackgroundPolicy(_)
        | Call::ReportClientActivity(_)
        | Call::RemoveClientActivity(_) => Reply::Background(peer.request(call).await?),
        Call::ReportHostPowerState(_) => {
            let _: m::Empty = peer.request(call).await?;
            Reply::Done
        }
        Call::ReadHostResources(_) => Reply::HostResources(peer.request(call).await?),
        Call::ReadProcessDiagnostics(_) => Reply::ProcessDiagnostics(peer.request(call).await?),
        Call::ReadProcessResourceHistory(_) => {
            Reply::ProcessResourceHistory(peer.request(call).await?)
        }
        Call::ReadTraceDiagnostics(_) => Reply::TraceDiagnostics(peer.request(call).await?),
        _ => {
            let _: m::Empty = peer.request(call).await?;
            Reply::Done
        }
    })
}

/// A turn diff shown as a workspace review.
pub fn turn_review(diff: c::TurnDiff) -> crate::models::WorkspaceReview {
    let mut review = crate::presentation::diff::review_from_patch(diff.diff);
    review.branch = format!("Turns {}–{}", diff.from_run_ordinal, diff.to_run_ordinal);
    review
}

impl Owner {
    pub(super) fn refresh(&mut self) {
        self.job(Call::ListProviders(m::Empty {}), None, None);
        self.job(Call::ReadSettings(m::Empty {}), None, None);
        self.refresh_accounts_if_due(super::owner::now_ms());
    }

    /// A replacement Network keeps the five-minute quota attempt throttle; a
    /// first connection still refreshes immediately because no attempt exists.
    pub(super) fn refresh_after_attach(&mut self) {
        self.refresh();
    }

    pub(super) fn job(
        &mut self,
        call: Call,
        complete: Option<Waiter>,
        sent: Option<(String, Draft)>,
    ) {
        let account_request = matches!(&call, Call::ListAccounts(_));
        let previous_attempt = self.usage_refresh_last_attempt_ms;
        if account_request {
            if self.accounts_refresh_in_flight() {
                if let Some(complete) = complete {
                    let _ = complete.send(Err(invalid("Account refresh already in progress")));
                }
                return;
            }
            self.accounts_refresh_in_flight_epoch = Some(self.epoch);
            self.usage_refresh_last_attempt_ms = Some(super::owner::now_ms());
        }
        let diff_generation = matches!(&call, Call::DiffPreview(_))
            .then_some(self.state.sources.diff_generation);
        let sender = self.sender.clone();
        let cancel = match &call {
            Call::Transcribe(params) => params
                .preparation
                .as_ref()
                .and_then(|id| self.dictations.get(id))
                .cloned(),
            _ => None,
        }
        .unwrap_or_default();
        let network = match self.network() {
            Ok(network) => network,
            Err(error) => {
                self.preview_recording_failed(&call);
                if account_request {
                    self.accounts_refresh_in_flight_epoch = None;
                    self.usage_refresh_last_attempt_ms = previous_attempt;
                }
                if let Some(complete) = complete {
                    self.state.error = Some(error.to_string());
                    let _ = complete.send(Err(error));
                }
                return;
            }
        };
        let (peer, epoch) = (network.peer.clone(), network.epoch);
        network.spawn(async move {
            let result = tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(invalid("Request cancelled")),
                result = execute(&peer, &call) => result,
            };
            let _ = sender
                .send(Event::Finished(
                    epoch,
                    Box::new(JobResult {
                        call,
                        result,
                        complete,
                        sent,
                        diff_generation,
                    }),
                ))
                .await;
        });
    }

    pub(super) fn pair(&mut self, invitation: m::Invitation, name: String, complete: Waiter) {
        let fail = |owner: &mut Self, complete: Waiter, error: PeerError| {
            owner.state.error = Some(error.to_string());
            let _ = complete.send(Err(error));
        };
        if super::owner::now_ms() / 1000 >= invitation.expires_at {
            return fail(self, complete, invalid("Invitation expired"));
        }
        let ticket = match invitation.endpoint.parse::<crate::transport::Ticket>() {
            Ok(ticket) => ticket,
            Err(error) => return fail(self, complete, invalid(error)),
        };
        let sender = self.sender.clone();
        let network = match self.network() {
            Ok(network) => network,
            Err(error) => return fail(self, complete, error),
        };
        let (session, peer, epoch) = (network.session.clone(), network.peer.clone(), network.epoch);
        let call = Call::RegisterRemote(op::RegisterRemoteHost {
            ticket: ticket.to_string(),
            name,
        });
        let code = invitation.invitation;
        network.spawn(async move {
            let result = match crate::client::pair_remote(&session, &ticket, code).await {
                Ok(()) => execute(&peer, &call).await,
                Err(error) => Err(error),
            };
            let _ = sender
                .send(Event::Finished(
                    epoch,
                    Box::new(JobResult {
                        call,
                        result,
                        complete: Some(complete),
                        sent: None,
                        diff_generation: None,
                    }),
                ))
                .await;
        });
    }

    pub(super) fn browser(
        &mut self,
        request: crate::browser::BrowserRequest,
        complete: oneshot::Sender<Result<crate::browser::BrowserFrame, PeerError>>,
    ) {
        let Ok(network) = self.network() else {
            let _ = complete.send(Err(invalid("Connect to the Host")));
            return;
        };
        let peer = network.peer.clone();
        network.spawn(async move {
            // The Host validates pointer coordinates against the page's
            // resource-owned viewport. Client-side fixed-size validation would
            // reject valid device presets before the Host sees them.
            let result = peer.request(&Call::Browser(request)).await;
            let _ = complete.send(result);
        });
    }

    pub(super) fn dictation(&mut self, id: String, cancel: CancellationToken) {
        self.dictations.retain(|_, token| !token.is_cancelled());
        self.dictations.insert(id.clone(), cancel.clone());
        if let Some(network) = &self.network {
            tokio::spawn(crate::client::prepare_dictation(
                network.peer.clone(),
                id,
                cancel,
            ));
        }
    }

    pub(super) fn transfer(
        &mut self,
        transfer: FileTransfer,
        complete: oneshot::Sender<Result<String, PeerError>>,
    ) {
        let Some(network) = self.network.as_mut() else {
            let _ = complete.send(Err(invalid("Connect to the Host")));
            return;
        };
        let (peer, session) = (network.peer.clone(), network.session.clone());
        network.spawn(async move {
            let open = || async { session.open_stream().await.map_err(std::io::Error::other) };
            let result = match transfer {
                FileTransfer::AttachmentDownload { id, destination } => {
                    match peer.request::<String>(&Call::AttachmentPath(id)).await {
                        Ok(source) => agent_transport::transfers::download_file(
                            &peer,
                            open,
                            std::path::Path::new(&source),
                            std::path::Path::new(&destination),
                        )
                        .await
                        .map(|_| destination),
                        Err(error) => Err(agent_transport::transfers::TransferError::Peer(error)),
                    }
                }
                FileTransfer::Download {
                    source,
                    destination,
                } => agent_transport::transfers::download_file(
                    &peer,
                    open,
                    std::path::Path::new(&source),
                    std::path::Path::new(&destination),
                )
                .await
                .map(|_| destination),
                FileTransfer::Upload {
                    source,
                    directory,
                    file_name,
                } => agent_transport::transfers::upload_file(
                    &peer,
                    open,
                    std::path::Path::new(&source),
                    std::path::Path::new(&directory),
                    &file_name,
                )
                .await
                .map(|file| file.path),
            }
            .map_err(invalid);
            let _ = complete.send(result);
        });
    }

    pub(super) fn finished(&mut self, result: JobResult) {
        let JobResult {
            call,
            result,
            complete,
            sent,
            diff_generation,
        } = result;
        let cancelled = match &call {
            Call::Transcribe(params) => params
                .preparation
                .as_ref()
                .and_then(|id| self.dictations.remove(id))
                .is_some_and(|token| token.is_cancelled()),
            _ => false,
        };
        let result = if cancelled {
            Err(invalid("Operation cancelled"))
        } else {
            result
        };
        if matches!(&call, Call::ReadHostResources(_)) {
            self.load_balancing_resources_in_flight = false;
        }
        let paired = match &result {
            Ok(Reply::Remote(host)) => Some(Outcome::RemoteHostPaired {
                id: host.id.clone(),
            }),
            Ok(Reply::PullResult(result)) => Some(Outcome::GitPulled {
                result: GitPullOutcome {
                    status: match result.status {
                        agent_protocol::vcs::PullStatus::Pulled => "pulled".into(),
                        agent_protocol::vcs::PullStatus::SkippedUpToDate => "skipped_up_to_date".into(),
                    },
                    ref_name: result.ref_name.clone(),
                    upstream_ref: result.upstream_ref.clone(),
                },
            }),
            Ok(Reply::CreatedWorktree(result)) => Some(Outcome::GitWorktreeCreated {
                result: GitWorktreeOutcome {
                    path: result.worktree.path.clone(),
                    ref_name: result.worktree.ref_name.clone(),
                },
            }),
            Ok(Reply::ResolvedPullRequest(result)) => Some(Outcome::GitPullRequestResolved {
                result: GitPullRequestOutcome {
                    number: result.pull_request.number,
                    title: result.pull_request.title.clone(),
                    url: result.pull_request.url.clone(),
                    base_branch: result.pull_request.base_branch.clone(),
                    head_branch: result.pull_request.head_branch.clone(),
                    state: format!("{:?}", result.pull_request.state).to_lowercase(),
                },
            }),
            Ok(Reply::PreparedPullRequestThread(result)) => {
                let pull_request = &result.pull_request;
                Some(Outcome::GitPullRequestThreadPrepared {
                    result: GitPullRequestThreadOutcome {
                        pull_request: GitPullRequestOutcome {
                            number: pull_request.number,
                            title: pull_request.title.clone(),
                            url: pull_request.url.clone(),
                            base_branch: pull_request.base_branch.clone(),
                            head_branch: pull_request.head_branch.clone(),
                            state: format!("{:?}", pull_request.state).to_lowercase(),
                        },
                        branch: result.branch.clone(),
                        worktree_path: result.worktree_path.clone(),
                        is_on_pull_request_head: result.is_on_pull_request_head,
                    },
                })
            }
            Ok(Reply::PublishedRepository(result)) => Some(Outcome::GitRepositoryPublished {
                result: GitPublishOutcome {
                    name_with_owner: result.repository.name_with_owner.clone(),
                    url: result.repository.url.clone(),
                    ssh_url: result.repository.ssh_url.clone(),
                    remote_name: result.remote_name.clone(),
                    remote_url: result.remote_url.clone(),
                    branch: result.branch.clone(),
                    upstream_branch: result.upstream_branch.clone(),
                    status: match result.status {
                        agent_protocol::vcs::PublishStatus::Pushed => "pushed".into(),
                        agent_protocol::vcs::PublishStatus::RemoteAdded => "remote_added".into(),
                    },
                },
            }),
            Ok(_) => match &call {
                Call::StartTerminal(params) => {
                    self.state.terminals.get(&params.handle()).map(|terminal| {
                        Outcome::TerminalOpened {
                            terminal_id: terminal.terminal_id.clone(),
                        }
                    })
                }
                _ => None,
            },
            Err(_) => None,
        };
        let outcome = match result {
            Err(error) => {
                self.preview_recording_failed(&call);
                if !cancelled
                    && (complete.is_some()
                        || matches!(
                            call,
                            Call::StartTerminal(_) | Call::RestartTerminal(_) | Call::Transcribe(_)
                        ))
                {
                    self.state.error = Some(crate::presentation::error::error_message(
                        &error.to_string(),
                    ));
                }
                if matches!(call, Call::ReadUsageSummary(_) | Call::RefreshUsageRates(_)) {
                    self.state.usage_loading = false;
                    self.state.usage_error = Some(error.to_string());
                }
                if matches!(
                    call,
                    Call::DeviceList(_)
                        | Call::DeviceConfigure(_)
                        | Call::DeviceHosts(_)
                        | Call::DeviceOpen(_)
                        | Call::DeviceClose(_)
                        | Call::DeviceShutdown(_)
                        | Call::DeviceDetail(_)
                        | Call::DeviceAction(_)
                        | Call::DeviceScreenshot(_)
                        | Call::DeviceInput(_)
                        | Call::DeviceAccessibility(_)
                        | Call::DeviceEventLog(_)
                        | Call::DeviceRecordingStart(_)
                        | Call::DeviceRecordingStop(_)
                ) {
                    self.state.device.error = Some(
                        crate::presentation::error::error_message(&error.to_string()),
                    );
                }
                if let Call::DeviceInput(request) = &call
                    && let Some(thread_id) = self.state.selected_thread.clone()
                {
                    self.state
                        .device
                        .fail_duo_for_input(&thread_id, request, error.to_string());
                }
                match &call {
                    Call::ProviderCommands(request) => {
                        self.provider_commands_finished(request, Err(&error))
                    }
                    Call::SearchAcpRegistry(request)
                        if request.query == self.state.acp_registry.query =>
                    {
                        self.state.acp_registry.search_pending = false;
                        self.state.acp_registry.error = Some(error.to_string());
                    }
                    Call::PrepareAcpAgent(request)
                        if self.state.acp_registry.prepare_pending.as_deref()
                            == Some(request.agent_id.as_str()) =>
                    {
                        self.state.acp_registry.prepare_pending = None;
                        self.state.acp_registry.error = Some(error.to_string());
                    }
                    Call::UninstallAcpAgent(request)
                        if self.state.acp_registry.uninstall_pending.as_deref()
                            == Some(request.agent_id.as_str()) =>
                    {
                        self.state.acp_registry.uninstall_pending = None;
                        self.state.acp_registry.error = Some(error.to_string());
                    }
                    Call::ProbeAcpAgent(request)
                        if self.state.acp_registry.probe_pending.as_deref()
                            == Some(request.agent_id.as_str()) =>
                    {
                        self.state.acp_registry.probe_pending = None;
                        self.state.acp_registry.error = Some(error.to_string());
                    }
                    Call::ListRefs(request) => self.refs_finished(request, Err(&error)),
                    Call::DiffPreview(request) if request.file.is_some() => {
                        self.diff_file_finished(request, Err(&error), diff_generation)
                    }
                    Call::DiffPreview(request) => {
                        self.diff_preview_finished(request, Err(&error), diff_generation);
                        self.retry_diff_preview_at_environment_cwd(request, &error);
                    }
                    Call::Search(params) => self.search_finished(&params.query, None),
                    Call::ListPullRequests(request) => {
                        self.state
                            .pull_requests
                            .list_requested
                            .remove(&request.project_id);
                    }
                    Call::SearchContents(request) => self.content_search_finished(request, Err(&error)),
                    Call::CancelSetup(_) => self.work_locally = None,
                    Call::ProjectFavicon(request) => self.project_icon_read(request, Err(())),
                    _ => {}
                }
                if let Call::ScanAgentSessions(_) = &call {
                    let import = &mut self.state.session_import;
                    import.scan_pending = false;
                    import.scan_error = Some(crate::presentation::error::error_message(
                        &error.to_string(),
                    ));
                }
                let failed_terminal = match &call {
                    Call::StartTerminal(params) => Some(params.handle()),
                    Call::RestartTerminal(params) => Some(params.handle()),
                    _ => None,
                };
                if let Some(handle) = failed_terminal
                    && let Some(terminal) = self.state.terminals.get_mut(&handle)
                {
                    terminal.phase = TerminalPhase::Failed(
                        crate::presentation::error::error_message(&error.to_string()),
                    );
                }
                Err(error)
            }
            Ok(reply) => {
                if complete.is_some() {
                    self.state.error = None;
                }
                self.reply(&call, reply, sent, diff_generation);
                Ok(paired.unwrap_or_default())
            }
        };
        if let Some(complete) = complete {
            let _ = complete.send(outcome);
        }
        if matches!(&call, Call::ListAccounts(_)) {
            self.account_refresh_finished();
        }
    }

    fn preview_recording_failed(&mut self, call: &Call) {
        let (tab_id, clear_artifact) = match call {
            Call::PreviewRecordingStart(request) => (&request.tab_id, true),
            Call::PreviewRecordingStop(request) => (&request.tab_id, false),
            _ => return,
        };
        self.state.preview.recordings.insert(
            tab_id.clone(),
            agent_protocol::preview::PreviewRecordingStatus {
                tab_id: tab_id.clone(),
                recording: false,
                started_at: None,
            },
        );
        if clear_artifact {
            self.state.preview.last_recordings.remove(tab_id);
        }
    }

    fn reply(
        &mut self,
        call: &Call,
        reply: Reply,
        sent: Option<(String, Draft)>,
        diff_generation: Option<u64>,
    ) {
        let workspace = &mut self.state.workspace;
        match reply {
            Reply::Providers(providers) => {
                let current_available = providers.iter().any(|instance| {
                    instance.instance == self.state.default_draft.instance_id
                        && instance.driver == self.state.default_draft.driver
                        && instance.enabled
                        && instance.installed
                        && instance.unavailable_reason.is_none()
                        && !matches!(
                            instance.status,
                            crate::models::ProviderStatus::Error
                                | crate::models::ProviderStatus::Disabled
                        )
                        && instance
                            .models
                            .iter()
                            .any(|model| model.slug == self.state.default_draft.model)
                });
                if (!current_available || self.state.default_draft.model.is_empty())
                    && let Some((instance, model)) = crate::view::models::default_model(&providers)
                {
                    let runtime_mode = self.state.default_draft.runtime_mode;
                    let interaction_mode = self.state.default_draft.interaction_mode;
                    self.state.default_draft = Draft {
                        instance_id: instance.instance.clone(),
                        driver: instance.driver,
                        model: model.slug.clone(),
                        runtime_mode,
                        interaction_mode,
                        ..Draft::default()
                    };
                }
                self.state.providers = Some(providers);
            }
            Reply::Search(matches) => {
                if let Call::Search(params) = call {
                    self.search_finished(&params.query, Some(matches));
                }
            }
            Reply::ProjectAdded(id) => self.state.selected_project = Some(id),
            Reply::Files(files) => {
                if let Call::ListFiles(request) = call
                    && workspace.requested_directory.as_ref() == Some(&request.path)
                {
                    workspace.directory = Some(files);
                    workspace.listed_directory = Some(request.path.clone());
                }
            }
            Reply::File(file) => {
                if let Call::WriteFile(written) = call
                    && let Some(draft) = workspace.file_drafts.get_mut(&file.path)
                    && draft.revision == written.revision
                {
                    if draft.text == written.text {
                        workspace.file_drafts.remove(&file.path);
                    } else {
                        Arc::make_mut(draft).revision = file.revision.clone();
                    }
                }
                let requested = match call {
                    Call::ReadFile(request) => workspace.requested_file.as_ref().map_or_else(
                        || {
                            workspace
                                .file
                                .as_ref()
                                .is_some_and(|current| current.path == file.path)
                        },
                        |path| path == &request.path,
                    ),
                    Call::WriteFile(_) => workspace
                        .file
                        .as_ref()
                        .is_some_and(|current| current.path == file.path),
                    _ => false,
                };
                if requested {
                    workspace.requested_file = Some(file.path.clone());
                    workspace.file = Some(Arc::new(file));
                }
            }
            Reply::Review(review) => {
                if let Call::ReviewWorkspace(request) = call
                    && request.cwd == self.state.cwd()
                    && self.state.workspace.diff_request.is_none()
                {
                    self.state.workspace.review_generation += 1;
                    self.state.workspace.review = Some(Arc::new(review));
                }
            }
            Reply::TurnDiff(diff) => {
                if let Call::GetTurnDiff(request) = call
                    && self.state.workspace.diff_request.as_ref() == Some(request)
                    && self.state.selected_thread.as_ref() == Some(&diff.thread_id)
                    && request.from_run_ordinal == diff.from_run_ordinal
                    && request.to_run_ordinal == diff.to_run_ordinal
                {
                    self.state.workspace.review_generation += 1;
                    self.state.workspace.review = Some(Arc::new(turn_review(diff)));
                }
            }
            Reply::WorktreeSettings(settings) => workspace.worktree_settings = Some(settings),
            Reply::Worktrees(worktrees) => workspace.worktrees = worktrees,
            Reply::Accounts(accounts) => self.state.accounts = Some(accounts),
            Reply::UsageSummary(summary) => {
                self.state.usage_loading = false;
                self.state.usage_error = None;
                self.state.usage_pricing = Some(summary.pricing.clone());
                self.state.usage_summary = Some(summary);
            }
            Reply::UsagePricing(pricing) => {
                self.state.usage_loading = false;
                self.state.usage_pricing = Some(pricing);
            }
            Reply::Login(login) => self.state.account_login = Some(login),
            Reply::HostStatus(status) => self.state.host_status = Some(status),
            Reply::UpdateStatus(status) => {
                self.state.updates.insert(status.target, status);
            }
            Reply::NativeUpdate(status) => self.state.native_update = Some(status),
            Reply::Remotes(remotes) => self.state.remote_hosts = remotes,
            Reply::Remote(host) => {
                self.state.remote_hosts.retain(|old| old.id != host.id);
                self.state.remote_hosts.push(host);
            }
            Reply::Invitation(invitation) => self.state.invitation = Some(invitation),
            Reply::PreviewList(result) => self.state.preview.apply_list(result),
            Reply::PreviewSession(session) => self.state.preview.upsert(session),
            Reply::PreviewRecordingStatus(status) => {
                self.state
                    .preview
                    .last_recordings
                    .remove(&status.tab_id);
                self.state
                    .preview
                    .recordings
                    .insert(status.tab_id.clone(), status);
            }
            Reply::PreviewRecordingArtifact(artifact) => {
                self.state.preview.recordings.insert(
                    artifact.tab_id.clone(),
                    agent_protocol::preview::PreviewRecordingStatus {
                        tab_id: artifact.tab_id.clone(),
                        recording: false,
                        started_at: None,
                    },
                );
                self.state
                    .preview
                    .last_recordings
                    .insert(artifact.tab_id.clone(), artifact);
            }
            Reply::Environment(environment) => {
                self.state.host_name = Some(environment.label.clone());
                self.state.environment = Some(environment);
            }
            Reply::AwarenessRegistration(_) => {}
            Reply::HostSettings(settings) => {
                let mut defaults = self.state.default_draft.user_defaults();
                defaults.runtime_mode = settings.default_runtime_mode;
                if let Some(selection) = &settings.default_model_selection {
                    defaults.instance_id = selection.instance.clone();
                    defaults.driver = selection.driver;
                    defaults.model = selection.model.clone();
                    defaults.options = selection
                        .options
                        .iter()
                        .map(|(key, value)| ModelOption {
                            key: key.clone(),
                            value: value.clone(),
                        })
                        .collect();
                }
                self.state.default_draft = defaults;
                self.state.host_settings = Some(settings);
                if matches!(call, Call::UpdateSettings(_)) {
                    self.job(Call::ListProviders(m::Empty {}), None, None);
                }
            }
            Reply::Keybindings(config) => self.state.keybindings = Some(Arc::new(config)),
            Reply::Background(snapshot) => self.state.background_policy = Some(snapshot),
            Reply::HostResources(resources) => {
                self.state.host_resources = Some(resources);
                self.state.host_resources_received_at_ms = Some(super::owner::now_ms() as i64);
                self.load_balancing_resources_in_flight = false;
            }
            Reply::ProcessDiagnostics(processes) => {
                self.state.process_diagnostics = Some(processes)
            }
            Reply::ProcessResourceHistory(history) => {
                self.state.process_resource_history = Some(history)
            }
            Reply::TraceDiagnostics(trace) => self.state.trace_diagnostics = Some(trace),
            Reply::SessionScan(scan) => {
                let import = &mut self.state.session_import;
                import.scan_pending = false;
                import.scan_error = None;
                import.scan = Some(scan);
                import.selection = None;
            }
            Reply::Transcription(text) => {
                if let Some((key, original)) = sent {
                    let draft = self.state.drafts.entry(key).or_insert(original);
                    if !draft.text.is_empty() && !text.is_empty() {
                        draft.text.push('\n');
                    }
                    draft.text.push_str(&text);
                }
            }
            Reply::ProviderCommands(commands) => {
                if let Call::ProviderCommands(request) = call {
                    self.provider_commands_finished(request, Ok(commands));
                }
            }
            Reply::ProviderUpdate(_) => {}
            Reply::AcpRegistrySearch(result) => {
                if let Call::SearchAcpRegistry(request) = call
                    && request.query == self.state.acp_registry.query
                {
                    self.state.acp_registry.search_pending = false;
                    self.state.acp_registry.results = Some(result);
                    self.state.acp_registry.error = None;
                }
            }
            Reply::PreparedAcpAgent(result) => {
                if let Call::PrepareAcpAgent(request) = call
                    && self.state.acp_registry.prepare_pending.as_deref()
                        == Some(request.agent_id.as_str())
                    && request.agent_id == result.agent_id
                {
                    self.state.acp_registry.prepare_pending = None;
                    self.state
                        .acp_registry
                        .prepared
                        .insert(result.agent_id.clone(), result);
                    self.state.acp_registry.error = None;
                }
            }
            Reply::UninstalledAcpAgent(result) => {
                if let Call::UninstallAcpAgent(request) = call
                    && self.state.acp_registry.uninstall_pending.as_deref()
                        == Some(request.agent_id.as_str())
                    && request.agent_id == result.agent_id
                {
                    self.state.acp_registry.uninstall_pending = None;
                    if result.removed {
                        self.state.acp_registry.prepared.remove(&result.agent_id);
                        self.state.acp_registry.probes.remove(&result.agent_id);
                    }
                    self.state.acp_registry.error = None;
                }
            }
            Reply::AcpProbe(result) => {
                if let Call::ProbeAcpAgent(request) = call
                    && self.state.acp_registry.probe_pending.as_deref()
                        == Some(request.agent_id.as_str())
                    && request.agent_id == result.agent_id
                {
                    self.state.acp_registry.probe_pending = None;
                    self.state
                        .acp_registry
                        .probes
                        .insert(result.agent_id.clone(), result);
                    self.state.acp_registry.error = None;
                }
            }
            Reply::EntrySearch(found) => {
                if let Call::SearchEntries(request) = call {
                    self.entries_found(request, found);
                }
            }
            Reply::ContentSearch(found) => {
                if let Call::SearchContents(request) = call {
                    self.content_search_finished(request, Ok(found));
                }
            }
            Reply::VcsStatus(status) => {
                if let Some(cwd) = match call {
                    Call::VcsStatus(request) => Some(request.cwd.clone()),
                    Call::RefreshVcsStatus(request) => Some(request.cwd.clone()),
                    _ => None,
                } {
                    self.state
                        .git
                        .status
                        .insert(cwd, status);
                }
            }
            Reply::PullResult(_)
            | Reply::CreatedWorktree(_)
            | Reply::ResolvedPullRequest(_)
            | Reply::PreparedPullRequestThread(_)
            | Reply::PublishedRepository(_) => {}
            Reply::Refs(list) => {
                if let Call::ListRefs(request) = call {
                    self.refs_finished(request, Ok(list));
                }
            }
            Reply::DiffPreview(preview) => {
                if let Call::DiffPreview(request) = call {
                    if request.file.is_some() {
                        self.diff_file_finished(request, Ok(preview), diff_generation);
                    } else {
                        self.diff_preview_finished(request, Ok(preview), diff_generation);
                        self.show_diff_preview();
                    }
                }
            }
            Reply::PullRequestList(list) => {
                if let Call::ListPullRequests(request) = call {
                    self.state
                        .pull_requests
                        .list_requested
                        .remove(&request.project_id);
                    self.state
                        .pull_requests
                        .by_project
                        .insert(request.project_id.clone(), list.entries);
                    self.state.pull_requests.selected_project = Some(request.project_id.clone());
                }
            }
            Reply::PullRequestDetail(detail) => {
                if let Call::GetPullRequest(request) = call {
                    self.state
                        .pull_requests
                        .details
                        .insert(request.reference.key().canonical(), detail);
                }
            }
            Reply::PullRequestDiff(diff) => {
                let key = diff.reference.key().canonical();
                let append = matches!(call, Call::GetPullRequestDiff(request) if request.cursor.is_some());
                if append {
                    if let Some(previous) = self.state.pull_requests.diffs.get_mut(&key) {
                        previous.files.extend(diff.files);
                        previous.patch.push_str(&diff.patch);
                        previous.truncated |= diff.truncated;
                        previous.next_cursor = diff.next_cursor;
                        if let Some(stats) = diff.omitted_file_stats {
                            previous
                                .omitted_file_stats
                                .get_or_insert_with(Vec::new)
                                .extend(stats);
                        }
                    } else {
                        self.state.pull_requests.diffs.insert(key, diff);
                    }
                } else {
                    let prefix = format!("{key}\0");
                    self.state
                        .pull_requests
                        .diff_file_contents
                        .retain(|context_key, _| !context_key.starts_with(&prefix));
                    self.state.pull_requests.diffs.insert(key, diff);
                }
            }
            Reply::PullRequestDiffFileContents(contents) => {
                if let Call::GetPullRequestDiffFileContents(request) = call {
                    let key = pull_request_diff_context_key(
                        &request.reference.key(),
                        &request.old_path,
                        &request.new_path,
                    );
                    self.state
                        .pull_requests
                        .diff_file_contents
                        .insert(key, contents);
                }
            }
            Reply::PullRequestFile(file) => {
                self.state
                    .pull_requests
                    .files
                    .insert(format!("{}:{}", file.reference.key().canonical(), file.path), file);
            }
            Reply::PullRequestViewedFiles(viewed) => {
                self.state
                    .pull_requests
                    .viewed_files
                    .insert(viewed.reference.key().canonical(), viewed);
            }
            Reply::PullRequestOperation(operation) => {
                if let Some(thread) = pull_request_operation_thread(call) {
                    self.state
                        .pull_requests
                        .links_by_thread
                        .insert(thread, operation.linked.clone());
                }
                if let Some(detail) = operation.detail {
                    self.state
                        .pull_requests
                        .details
                        .insert(operation.reference.key().canonical(), detail);
                }
            }
            Reply::PullRequestAuth(auth) => self.state.pull_requests.auth = Some(auth),
            Reply::PullRequestDiscovery(discovery) => {
                self.state.pull_requests.discovery = Some(discovery)
            }
            Reply::DeviceState(service) => self.state.device.apply_event(d::DeviceEvent::State(service)),
            Reply::DeviceSession(session) => {
                self.state.device.sessions.retain(|existing| {
                    !(existing.thread_id == session.thread_id
                        && existing.host_id == session.host_id
                        && existing.device_id == session.device_id)
                });
                self.state.device.sessions.push(session);
            }
            Reply::DeviceDetail(detail) => {
                self.state
                    .device
                    .details
                    .insert((detail.host_id.clone(), detail.device_id.clone()), detail);
            }
            Reply::DeviceScreenshot(screenshot) => {
                self.state.device.last_screenshot = Some(screenshot);
            }
            Reply::DeviceAccessibility(tree) => {
                self.state.device.apply_event(d::DeviceEvent::Accessibility(tree));
            }
            Reply::DeviceEventLog(entries) => {
                for entry in entries {
                    self.state.device.apply_event(d::DeviceEvent::EventLog(entry));
                }
            }
            Reply::DeviceRecordingStatus(status) => {
                self.state.device.apply_event(d::DeviceEvent::Recording(status));
            }
            Reply::DeviceRecording(recording) => {
                self.state.device.apply_event(d::DeviceEvent::RecordingComplete(recording));
            }
            Reply::SwitchedRef(switched) => match call {
                Call::SwitchRef(request) => {
                    self.switched_ref(request, switched);
                    self.load_refs(request.cwd.clone(), RefScope::All, String::new());
                }
                Call::CreateRef(request) => {
                    self.switched_ref(
                        &w::SwitchRef {
                            cwd: request.cwd.clone(),
                            ref_name: request.ref_name.clone(),
                        },
                        switched,
                    );
                    self.load_refs(request.cwd.clone(), RefScope::All, String::new());
                }
                _ => {}
            },
            Reply::ProjectIcon(favicon) => {
                if let Call::ProjectFavicon(request) = call {
                    self.project_icon_read(request, Ok(favicon));
                }
            }
            Reply::SetupCancelled(cancelled) => {
                if let Call::CancelSetup(request) = call {
                    self.setup_cancelled(&request.thread_id, cancelled.cancelled);
                }
            }
            Reply::ScheduledTask(task) => {
                self.state
                    .scheduled_tasks
                    .retain(|existing| existing.id != task.id);
                self.state.scheduled_tasks.push(task);
            }
            Reply::ScheduledTasks(tasks) => {
                self.state.scheduled_tasks = tasks.tasks;
            }
            Reply::ScheduledTaskRef(task) => {
                self.state
                    .scheduled_tasks
                    .retain(|existing| existing.id != task.id);
            }
            Reply::Done => {
                if let Call::DeviceInput(request) = call
                    && let Some(thread_id) = self.state.selected_thread.clone()
                    && let Some(next) = self.state.device.complete_duo_for_input(&thread_id, request)
                {
                    self.job(
                        Call::DeviceInput(d::DeviceInput {
                            host_id: next.host_id,
                            device_id: next.device_id,
                            input: d::DeviceInputKind::Duo {
                                command: super::intents::device_duo_command(next.command),
                            },
                        }),
                        None,
                        None,
                    );
                }
            }
        }
        match call {
            Call::RemoveRemote(params) => {
                self.state.remote_hosts.retain(|host| host.id != params.id)
            }
            Call::Revoke(_) => self.job(Call::HostStatus(m::Empty {}), None, None),
            Call::StartTerminal(params) => self.terminal_started(&params.handle()),
            Call::RestartTerminal(params) => self.terminal_started(&params.handle()),
            Call::ResizeTerminal(params) => {
                if let Some(terminal) = self.state.terminals.get_mut(&params.handle) {
                    terminal.size = params.size;
                }
            }
            Call::DetachTerminal(params) => {
                if let Some(terminal) = self.state.terminals.get_mut(&params.handle) {
                    terminal.phase = TerminalPhase::Detached;
                }
            }
            Call::PreviewClose(params) => self.state.preview.close(params.tab_id.as_deref()),
            Call::SelectAccount(_)
            | Call::SubmitAccountLogin(_)
            | Call::CancelAccountLogin(_)
            | Call::LogoutAccount(_) => self.refresh(),
            _ => {}
        }
    }
}

fn pull_request_operation_thread(call: &Call) -> Option<agent_domain::ThreadId> {
    let thread = match call {
        Call::LinkPullRequest(request) => &request.thread_id,
        Call::UnlinkPullRequest(request) => &request.thread_id,
        Call::SetPullRequestWatch(request) => &request.thread_id,
        _ => return None,
    };
    agent_domain::ThreadId::new(thread.clone()).ok()
}
