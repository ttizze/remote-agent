//! Requests outside the conversation stream: models, files, accounts,
//! terminals, transfers and pairing.
use super::{
    Outcome, Peer, invalid,
    owner::{Event, FileTransfer, Owner, Waiter},
};
use crate::{peer::PeerError, protocol::Call, state::*};
use agent_protocol::{conversation as c, device as d, models as m, operations as op, workspace as w};
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub(super) struct JobResult {
    pub call: Call,
    pub result: Result<Reply, PeerError>,
    pub complete: Option<Waiter>,
    pub sent: Option<(String, Draft)>,
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
    Login(op::AccountLogin),
    HostStatus(m::HostStatus),
    Remotes(Vec<m::RemoteHost>),
    Remote(m::RemoteHost),
    Invitation(m::Invitation),
    PreviewList(agent_protocol::preview::PreviewListResult),
    PreviewSession(agent_protocol::preview::PreviewSessionSnapshot),
    PreviewRecordingStatus(agent_protocol::preview::PreviewRecordingStatus),
    PreviewRecordingArtifact(agent_protocol::preview::PreviewRecordingArtifact),
    ContentSearch(agent_protocol::workspace::ContentSearch),
    Transcription(String),
    ConversationSettings(m::ConversationSettings),
    SessionScan(c::SessionScan),
    ProviderCommands(w::ProviderCommands),
    EntrySearch(w::EntrySearch),
    VcsStatus(w::VcsStatus),
    Refs(w::RefList),
    DiffPreview(w::DiffPreviewResult),
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
        Call::ReadConversationSettings(_) | Call::UpdateConversationSettings(_) => {
            Reply::ConversationSettings(peer.request(call).await?)
        }
        Call::ScanAgentSessions(_) => Reply::SessionScan(peer.request(call).await?),
        Call::ProviderCommands(_) => Reply::ProviderCommands(peer.request(call).await?),
        Call::SearchEntries(_) => Reply::EntrySearch(peer.request(call).await?),
        Call::VcsStatus(_) => Reply::VcsStatus(peer.request(call).await?),
        Call::ListRefs(_) => Reply::Refs(peer.request(call).await?),
        Call::DiffPreview(_) => Reply::DiffPreview(peer.request(call).await?),
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
        Call::StartAccountLogin(_) => Reply::Login(peer.request(call).await?),
        Call::HostStatus(_) => Reply::HostStatus(peer.request(call).await?),
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
        for call in [
            Call::ListProviders(m::Empty {}),
            Call::ListAccounts(m::Empty {}),
        ] {
            self.job(call, None, None);
        }
    }

    pub(super) fn job(
        &mut self,
        call: Call,
        complete: Option<Waiter>,
        sent: Option<(String, Draft)>,
    ) {
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
        let paired = match &result {
            Ok(Reply::Remote(host)) => Some(Outcome::RemoteHostPaired {
                id: host.id.clone(),
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
                match &call {
                    Call::ProviderCommands(request) => {
                        self.provider_commands_finished(request, Err(&error))
                    }
                    Call::ListRefs(request) => self.refs_finished(request, Err(&error)),
                    Call::DiffPreview(request) if request.file.is_some() => {
                        self.diff_file_finished(request, Err(&error))
                    }
                    Call::DiffPreview(request) => self.diff_preview_finished(request, Err(&error)),
                    Call::Search(params) => self.search_finished(&params.query, None),
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
                self.reply(&call, reply, sent);
                Ok(paired.unwrap_or_default())
            }
        };
        if let Some(complete) = complete {
            let _ = complete.send(outcome);
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

    fn reply(&mut self, call: &Call, reply: Reply, sent: Option<(String, Draft)>) {
        let workspace = &mut self.state.workspace;
        match reply {
            Reply::Providers(providers) => {
                if self.state.default_draft.model.is_empty()
                    && let Some((instance, model)) = crate::view::models::default_model(&providers)
                {
                    self.state.default_draft = Draft {
                        instance_id: instance.instance.clone(),
                        driver: instance.driver,
                        model: model.slug.clone(),
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
            Reply::Login(login) => self.state.account_login = Some(login),
            Reply::HostStatus(status) => self.state.host_status = Some(status),
            Reply::Remotes(remotes) => self.state.remote_hosts = remotes,
            Reply::Remote(host) => {
                self.state.remote_hosts.retain(|old| old.id != host.id);
                self.state.remote_hosts.push(host);
            }
            Reply::Invitation(invitation) => self.state.invitation = Some(invitation),
            Reply::PreviewList(result) => self.state.preview.apply_list(result),
            Reply::PreviewSession(session) => self.state.preview.upsert(session),
            Reply::PreviewRecordingStatus(status) => self.state.preview.apply_recording_status(status),
            Reply::PreviewRecordingArtifact(artifact) => self.state.preview.apply_recording_artifact(artifact),
            Reply::ConversationSettings(settings) => {
                self.state.conversation_settings = Some(settings)
            }
            Reply::Keybindings(config) => self.state.keybindings = Some(Arc::new(config)),
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
                if let Call::VcsStatus(request) = call {
                    self.state
                        .sources
                        .vcs_status
                        .insert(request.cwd.clone(), status);
                }
            }
            Reply::Refs(list) => {
                if let Call::ListRefs(request) = call {
                    self.refs_finished(request, Ok(list));
                }
            }
            Reply::DiffPreview(preview) => {
                if let Call::DiffPreview(request) = call {
                    if request.file.is_some() {
                        self.diff_file_finished(request, Ok(preview));
                    } else {
                        self.diff_preview_finished(request, Ok(preview));
                        self.show_diff_preview();
                    }
                }
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
                self.state.device.apply_event(d::DeviceEvent::Recording(recording.status.clone()));
                self.state.device.last_recording = Some(recording);
            }
            Reply::SwitchedRef(switched) => match call {
                Call::SwitchRef(request) => self.switched_ref(request, switched),
                Call::CreateRef(request) => self.switched_ref(
                    &w::SwitchRef {
                        cwd: request.cwd.clone(),
                        ref_name: request.ref_name.clone(),
                    },
                    switched,
                ),
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
            Reply::Done => {}
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
