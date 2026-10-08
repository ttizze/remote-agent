//! Native intents: device edits apply at once; Host changes go through the
//! outbox or a request.
use super::{
    Outcome,
    owner::{Event, Owner, Waiter, new_id},
};
use crate::{
    commands::{
        build::*,
        lifecycle::LifecycleOverlay,
        outbox::{PendingCommand, Request, Restore},
        workflows::{latest_merge_back_run, queue_workflow, sort_pinned_by_order},
    },
    peer::PeerError,
    protocol::Call,
    state::*,
    view::{
        attachments::{
            AttachmentCandidate, AttachmentFileKind, admit_attachments, image_preparation_error,
        },
        models::staging::remember_model_options,
        settings::{ProjectSettingKey, clear_project_overrides, plan_settings_update},
    },
};
use agent_domain::{
    ApprovalDecision, Command, InteractionMode, MessageId, Plan, PlanRef, RunId, RuntimeRequestId,
    State, ThreadId, Timestamp,
};
use agent_protocol::{
    conversation as c,
    device as d,
    models as m,
    operations as op,
    pull_requests as pr,
};
use std::sync::Arc;

pub(super) enum Next {
    Done,
    /// Outbox entries in order; the last one resolves the intent.
    Commands(Vec<PendingCommand>),
    /// A request whose reply resolves the intent.
    Call(Box<Call>, Option<Box<(String, Draft)>>),
    /// Applied at once with this outcome.
    Outcome(Outcome),
}
impl Next {
    pub(super) fn call(call: Call, sent: Option<(String, Draft)>) -> Self {
        Self::Call(Box::new(call), sent.map(Box::new))
    }
}

pub(super) fn invalid(error: impl std::fmt::Display) -> PeerError {
    super::invalid(error)
}
fn thread_id(value: String) -> Result<ThreadId, PeerError> {
    ThreadId::new(value).map_err(invalid)
}
fn run_id(value: String) -> Result<RunId, PeerError> {
    RunId::new(value).map_err(invalid)
}

fn device_platform(value: &str) -> Result<d::DevicePlatform, PeerError> {
    match value {
        "ios" => Ok(d::DevicePlatform::Ios),
        "android" => Ok(d::DevicePlatform::Android),
        _ => Err(invalid("Device platform must be ios or android")),
    }
}
fn device_text_size(value: &str) -> Result<d::DeviceTextSize, PeerError> {
    match value {
        "small" => Ok(d::DeviceTextSize::Small),
        "default" => Ok(d::DeviceTextSize::Default),
        "large" => Ok(d::DeviceTextSize::Large),
        "extraLarge" => Ok(d::DeviceTextSize::ExtraLarge),
        _ => Err(invalid("Unknown device text size")),
    }
}
fn device_color_filter(value: &str) -> Result<d::DeviceColorFilter, PeerError> {
    match value {
        "none" => Ok(d::DeviceColorFilter::None),
        "grayscale" => Ok(d::DeviceColorFilter::Grayscale),
        "redGreen" => Ok(d::DeviceColorFilter::RedGreen),
        "greenRed" => Ok(d::DeviceColorFilter::GreenRed),
        "blueYellow" => Ok(d::DeviceColorFilter::BlueYellow),
        _ => Err(invalid("Unknown device color filter")),
    }
}
fn device_orientation(value: &str) -> Result<d::DeviceOrientation, PeerError> {
    match value {
        "portrait" => Ok(d::DeviceOrientation::Portrait),
        "landscapeLeft" => Ok(d::DeviceOrientation::LandscapeLeft),
        "portraitUpsideDown" => Ok(d::DeviceOrientation::PortraitUpsideDown),
        "landscapeRight" => Ok(d::DeviceOrientation::LandscapeRight),
        _ => Err(invalid("Unknown device orientation")),
    }
}
fn device_fold_posture(value: DeviceFoldPostureIntent) -> d::DeviceFoldPosture {
    match value {
        DeviceFoldPostureIntent::Closed => d::DeviceFoldPosture::Closed,
        DeviceFoldPostureIntent::Opened => d::DeviceFoldPosture::Opened,
    }
}
fn device_duo_pose(value: DeviceDuoPoseIntent) -> d::DeviceDuoPose {
    match value {
        DeviceDuoPoseIntent::Closed => d::DeviceDuoPose::Closed,
        DeviceDuoPoseIntent::Book => d::DeviceDuoPose::Book,
        DeviceDuoPoseIntent::Open => d::DeviceDuoPose::Open,
        DeviceDuoPoseIntent::Laptop => d::DeviceDuoPose::Laptop,
        DeviceDuoPoseIntent::Tent => d::DeviceDuoPose::Tent,
    }
}
fn device_duo_physical(value: DeviceDuoPhysicalIntent) -> d::DeviceDuoPhysical {
    match value {
        DeviceDuoPhysicalIntent::Faceup => d::DeviceDuoPhysical::Faceup,
        DeviceDuoPhysicalIntent::Facedown => d::DeviceDuoPhysical::Facedown,
    }
}
fn device_duo_orientation(value: DeviceDuoOrientationIntent) -> d::DeviceOrientation {
    match value {
        DeviceDuoOrientationIntent::Portrait => d::DeviceOrientation::Portrait,
        DeviceDuoOrientationIntent::LandscapeLeft => d::DeviceOrientation::LandscapeLeft,
        DeviceDuoOrientationIntent::PortraitUpsideDown => d::DeviceOrientation::PortraitUpsideDown,
        DeviceDuoOrientationIntent::LandscapeRight => d::DeviceOrientation::LandscapeRight,
    }
}
fn device_duo_command(value: DeviceDuoCommandIntent) -> d::DeviceDuoCommand {
    match value {
        DeviceDuoCommandIntent::Angle { value } => d::DeviceDuoCommand::Angle { value },
        DeviceDuoCommandIntent::Pose { value } => d::DeviceDuoCommand::Pose { value: device_duo_pose(value) },
        DeviceDuoCommandIntent::Table { value } => d::DeviceDuoCommand::Table { value },
        DeviceDuoCommandIntent::Physical { value } => d::DeviceDuoCommand::Physical { value: device_duo_physical(value) },
        DeviceDuoCommandIntent::Orientation { value } => d::DeviceDuoCommand::Orientation { value: device_duo_orientation(value) },
    }
}
fn device_permission(value: &str) -> Result<d::DevicePermission, PeerError> {
    match value {
        "camera" => Ok(d::DevicePermission::Camera),
        "microphone" => Ok(d::DevicePermission::Microphone),
        "photos" => Ok(d::DevicePermission::Photos),
        "contacts" => Ok(d::DevicePermission::Contacts),
        "calendar" => Ok(d::DevicePermission::Calendar),
        "reminders" => Ok(d::DevicePermission::Reminders),
        "location" => Ok(d::DevicePermission::Location),
        "notifications" => Ok(d::DevicePermission::Notifications),
        "motion" => Ok(d::DevicePermission::Motion),
        "mediaLibrary" => Ok(d::DevicePermission::MediaLibrary),
        "faceId" => Ok(d::DevicePermission::FaceId),
        _ => Err(invalid("Unknown device permission")),
    }
}
fn device_permission_decision(value: &str) -> Result<d::DevicePermissionDecision, PeerError> {
    match value {
        "grant" => Ok(d::DevicePermissionDecision::Grant),
        "revoke" => Ok(d::DevicePermissionDecision::Revoke),
        "reset" => Ok(d::DevicePermissionDecision::Reset),
        _ => Err(invalid("Unknown device permission decision")),
    }
}
fn device_action(action: DeviceActionIntent) -> Result<d::DeviceActionKind, PeerError> {
    Ok(match action {
        DeviceActionIntent::SetAppearance { dark } => d::DeviceActionKind::SetAppearance(
            if dark { d::DeviceAppearance::Dark } else { d::DeviceAppearance::Light },
        ),
        DeviceActionIntent::SetTextSize { size } => d::DeviceActionKind::SetTextSize(device_text_size(&size)?),
        DeviceActionIntent::SetToggle { setting, value } => d::DeviceActionKind::SetToggle { setting, value },
        DeviceActionIntent::SetLiquidGlass { value } => d::DeviceActionKind::SetLiquidGlass(value),
        DeviceActionIntent::SetColorFilter { filter } => d::DeviceActionKind::SetColorFilter(device_color_filter(&filter)?),
        DeviceActionIntent::SetOrientation { orientation } => d::DeviceActionKind::SetOrientation(device_orientation(&orientation)?),
        DeviceActionIntent::SetLocation { latitude, longitude } => d::DeviceActionKind::SetLocation { latitude, longitude },
        DeviceActionIntent::ClearLocation => d::DeviceActionKind::ClearLocation,
        DeviceActionIntent::SetPermission { app_id, permission, decision } => d::DeviceActionKind::SetPermission {
            app_id,
            permission: device_permission(&permission)?,
            decision: device_permission_decision(&decision)?,
        },
        DeviceActionIntent::OpenUrl { url } => d::DeviceActionKind::OpenUrl(url),
        DeviceActionIntent::LaunchApp { app_id } => d::DeviceActionKind::LaunchApp(app_id),
        DeviceActionIntent::TerminateApp { app_id } => d::DeviceActionKind::TerminateApp(app_id),
        DeviceActionIntent::Shake => d::DeviceActionKind::Shake,
        DeviceActionIntent::SendPush { app_id, payload } => d::DeviceActionKind::SendPush {
            app_id,
            payload: serde_json::from_str(&payload).map_err(|_| invalid("Push payload must be JSON"))?,
        },
        DeviceActionIntent::Touch { phase, x, y } => d::DeviceActionKind::Input(d::DeviceInputKind::Touch { phase: device_touch_phase(&phase)?, x, y }),
        DeviceActionIntent::Key { code, key, down, meta, ctrl } => {
            d::DeviceActionKind::Input(d::DeviceInputKind::Key { code, key, down, meta, ctrl })
        }
        DeviceActionIntent::HardwareButton { button } => d::DeviceActionKind::Input(d::DeviceInputKind::HardwareButton(device_hardware_button(&button)?)),
        DeviceActionIntent::Rotate => d::DeviceActionKind::Input(d::DeviceInputKind::Rotate),
        DeviceActionIntent::Fold { command } => d::DeviceActionKind::Input(d::DeviceInputKind::Fold { command: device_fold_posture(command) }),
        DeviceActionIntent::Duo { command } => d::DeviceActionKind::Input(d::DeviceInputKind::Duo { command: device_duo_command(command) }),
    })
}
fn device_touch_phase(value: &str) -> Result<d::DeviceTouchPhase, PeerError> {
    match value { "begin" => Ok(d::DeviceTouchPhase::Begin), "move" => Ok(d::DeviceTouchPhase::Move), "end" => Ok(d::DeviceTouchPhase::End), _ => Err(invalid("Unknown device touch phase")) }
}
fn device_hardware_button(value: &str) -> Result<d::DeviceHardwareButton, PeerError> {
    match value { "home" => Ok(d::DeviceHardwareButton::Home), "back" => Ok(d::DeviceHardwareButton::Back), "recents" => Ok(d::DeviceHardwareButton::Recents), "power" => Ok(d::DeviceHardwareButton::Power), "appSwitcher" => Ok(d::DeviceHardwareButton::AppSwitcher), _ => Err(invalid("Unknown device hardware button")) }
}
fn device_recording_format(value: &str) -> Result<d::DeviceRecordingFormat, PeerError> {
    match value { "mp4" => Ok(d::DeviceRecordingFormat::Mp4), _ => Err(invalid("Unknown device recording format")) }
}
fn approval_decision(value: &str) -> Result<ApprovalDecision, PeerError> {
    Ok(match value {
        "accept" => ApprovalDecision::Accept,
        "acceptForSession" => ApprovalDecision::AcceptForSession,
        "acceptAlways" => ApprovalDecision::AcceptAlways,
        "decline" => ApprovalDecision::Decline,
        "cancel" => ApprovalDecision::Cancel,
        _ => return Err(invalid("Unknown approval decision")),
    })
}

/// The proposed plan the composer offers to implement or refine, by the
/// plan follow-up rules of the open thread and its draft.
pub(super) fn actionable_plan(snapshot: &Snapshot) -> Option<&Plan> {
    let view = crate::view::plan::selected_plan_view(snapshot)?;
    let id = view
        .active_proposed_plan
        .filter(|_| view.show_plan_follow_up_prompt)?
        .id;
    snapshot
        .selected_state()?
        .plans
        .iter()
        .find(|plan| plan.id.as_str() == id)
}

impl Owner {
    pub(super) fn intent(&mut self, intent: Intent, complete: Waiter) {
        match intent {
            Intent::PairRemoteHost { invitation, name } => {
                self.pair(invitation, name, complete);
                return;
            }
            Intent::ImportSessions => {
                self.import_sessions(complete);
                return;
            }
            _ => {}
        }
        let undoing = matches!(intent, Intent::UndoThreadAction);
        match self.prepare(intent) {
            Err(error) => {
                self.state.error = Some(crate::presentation::error::error_message(
                    &error.to_string(),
                ));
                let _ = complete.send(Err(error));
            }
            Ok(Next::Done) => {
                let _ = complete.send(Ok(Outcome::Applied));
            }
            Ok(Next::Outcome(outcome)) => {
                let _ = complete.send(Ok(outcome));
            }
            Ok(Next::Commands(mut entries)) => {
                if !undoing {
                    for entry in &entries {
                        self.claim_undo(entry);
                    }
                }
                let Some(last) = entries.pop() else {
                    let _ = complete.send(Ok(Outcome::Applied));
                    return;
                };
                for entry in entries {
                    if let Err(error) = self.enqueue(entry, None) {
                        let _ = complete.send(Err(error));
                        return;
                    }
                }
                let restore = last.restore.clone();
                if let Err(error) = self.enqueue(last, Some(complete)) {
                    if let Some(restore) = restore {
                        self.restore_draft(&restore);
                    }
                    self.state.error = Some(error.to_string());
                }
            }
            Ok(Next::Call(call, sent)) => self.job(*call, Some(complete), sent.map(|sent| *sent)),
        }
    }

    pub(super) fn command(&self, thread: ThreadId, command: Command) -> PendingCommand {
        self.pending(thread, command)
    }

    pub(super) fn lifecycle(&self, thread: ThreadId, action: LifecycleAction) -> PendingCommand {
        let overlay = LifecycleOverlay::of(&action);
        let mut entry = self.command(thread, lifecycle_command(action));
        entry.overlay = overlay;
        entry
    }

    pub(super) fn prepare(&mut self, intent: Intent) -> Result<Next, PeerError> {
        // The open draft is frozen as it was before this intent changes it.
        self.state.freeze_open_draft();
        let next = self.prepare_intent(intent);
        self.state
            .settle_new_thread_drafts(super::owner::now_ms() as i64);
        next
    }

    fn prepare_intent(&mut self, intent: Intent) -> Result<Next, PeerError> {
        Ok(match intent {
            Intent::OpenThread { thread_id: id } => {
                self.select_thread(Some(thread_id(id)?));
                Next::Done
            }
            Intent::OpenDraft { draft_key } => {
                let draft = self
                    .state
                    .drafts
                    .get(&draft_key)
                    .filter(|draft| !draft_key.is_empty() && draft.project_id.is_some())
                    .ok_or_else(|| invalid("Unknown draft"))?;
                let project_id = draft.project_id.clone();
                self.select_thread(None);
                self.state.open_new_thread_draft = Some(draft_key);
                self.state.selected_project = project_id
                    .filter(|project| project.as_str() != CHATS_PROJECT);
                Next::Done
            }
            Intent::LeaveThread => {
                self.select_thread(None);
                Next::Done
            }
            Intent::NewThread { project_id } => {
                self.select_thread(None);
                self.state.begin_new_thread_draft(
                    new_id("new"),
                    project_id,
                    super::owner::now_ms() as i64,
                );
                Next::Done
            }
            Intent::SetNewThreadProject { project_id } => {
                if self.state.selected_thread.is_some() {
                    return Err(invalid("Open a new thread first"));
                }
                self.ensure_new_thread_draft();
                self.state.retarget_new_thread_draft(project_id);
                Next::Done
            }
            Intent::ShowArchived { open } => {
                self.show_archived(open);
                Next::Done
            }
            Intent::FilterProject { project_id } => {
                self.state.selected_project = project_id;
                Next::Done
            }
            Intent::Search { query } => {
                self.search(query);
                Next::Done
            }
            Intent::LoadPullRequests {
                project_id,
                repository,
                query,
                include_closed,
            } => Next::call(
                Call::ListPullRequests(pr::ListPullRequests {
                    project_id,
                    repository,
                    host: None,
                    state: if include_closed {
                        pr::PullRequestListState::All
                    } else {
                        pr::PullRequestListState::Open
                    },
                    query,
                    limit: 100,
                    cursor: None,
                    fresh: true,
                }),
                None,
            ),
            Intent::LoadPullRequest {
                project_id,
                host,
                repository,
                number,
            } => Next::call(
                Call::GetPullRequest(pr::GetPullRequest {
                    reference: pr::PullRequestRef {
                        project_id,
                        repository,
                        number,
                        host,
                        allow_stale: false,
                    },
                }),
                None,
            ),
            Intent::LoadPullRequestDiff {
                project_id,
                host,
                repository,
                number,
                cursor,
                commit,
            } => Next::call(
                Call::GetPullRequestDiff(pr::GetPullRequestDiff {
                    reference: pr::PullRequestRef {
                        project_id,
                        repository,
                        number,
                        host,
                        allow_stale: false,
                    },
                    cursor,
                    commit,
                    fresh: true,
                }),
                None,
            ),
            Intent::LoadPullRequestDiffFileContents {
                project_id,
                host,
                repository,
                number,
                commit,
                change_type,
                old_path,
                new_path,
            } => Next::call(
                Call::GetPullRequestDiffFileContents(pr::GetPullRequestDiffFileContents {
                    reference: pr::PullRequestRef {
                        project_id,
                        repository,
                        number,
                        host,
                        allow_stale: false,
                    },
                    commit,
                    change_type: match change_type {
                        PullRequestDiffChangeTypeInput::Change => {
                            pr::PullRequestDiffChangeType::Change
                        }
                        PullRequestDiffChangeTypeInput::RenamePure => {
                            pr::PullRequestDiffChangeType::RenamePure
                        }
                        PullRequestDiffChangeTypeInput::RenameChanged => {
                            pr::PullRequestDiffChangeType::RenameChanged
                        }
                        PullRequestDiffChangeTypeInput::New => pr::PullRequestDiffChangeType::New,
                        PullRequestDiffChangeTypeInput::Deleted => {
                            pr::PullRequestDiffChangeType::Deleted
                        }
                    },
                    old_path,
                    new_path,
                }),
                None,
            ),
            Intent::LoadPullRequestViewedFiles {
                project_id,
                host,
                repository,
                number,
            } => Next::call(
                Call::GetPullRequestViewedFiles(pr::GetPullRequestViewedFiles {
                    reference: pr::PullRequestRef {
                        project_id,
                        repository,
                        number,
                        host,
                        allow_stale: false,
                    },
                    limit: 1_000,
                }),
                None,
            ),
            Intent::SetPullRequestFilesViewed {
                project_id,
                host,
                repository,
                number,
                files,
            } => Next::call(
                Call::SetPullRequestFilesViewed(pr::SetPullRequestFilesViewed {
                    reference: pr::PullRequestRef {
                        project_id,
                        repository,
                        number,
                        host,
                        allow_stale: false,
                    },
                    files: files
                        .into_iter()
                        .map(|file| pr::PullRequestViewedFile {
                            path: file.path,
                            viewed: file.viewed,
                        })
                        .collect(),
                }),
                None,
            ),
            Intent::PullRequestAction {
                project_id,
                host,
                repository,
                number,
                action,
                stack_number,
                expected_stack_heads,
                merge_method,
            } => Next::call(
                Call::PullRequestAction(pr::PullRequestActionRequest {
                    reference: pr::PullRequestRef {
                        project_id,
                        repository,
                        number,
                        host,
                        allow_stale: false,
                    },
                    action: parse_pull_request_action(&action)?,
                    stack_number,
                    expected_stack_heads: (!expected_stack_heads.is_empty()).then(|| {
                        expected_stack_heads
                            .into_iter()
                            .map(|head| pr::PullRequestStackHead {
                                number: head.number,
                                head_sha: head.head_sha,
                            })
                            .collect()
                    }),
                    merge_method: merge_method
                        .as_deref()
                        .map(parse_pull_request_merge_method)
                        .transpose()?,
                }),
                None,
            ),
            Intent::SubmitPullRequestReview {
                project_id,
                host,
                repository,
                number,
                verdict,
                body,
            } => Next::call(
                Call::SubmitPullRequestReview(pr::SubmitPullRequestReview {
                    reference: pr::PullRequestRef {
                        project_id,
                        repository,
                        number,
                        host,
                        allow_stale: false,
                    },
                    verdict: parse_pull_request_verdict(&verdict)?,
                    body,
                }),
                None,
            ),
            Intent::LinkPullRequest {
                thread_id,
                project_id,
                host,
                repository,
                number,
                url,
            } => Next::call(
                Call::LinkPullRequest(pr::LinkPullRequest {
                    thread_id,
                    project_id,
                    host,
                    repository,
                    number,
                    url,
                    source: agent_domain::PullRequestLinkSource::Manual,
                    refresh: true,
                }),
                None,
            ),
            Intent::UnlinkPullRequest {
                thread_id,
                project_id,
                host,
                repository,
                number,
            } => Next::call(
                Call::UnlinkPullRequest(pr::UnlinkPullRequest {
                    thread_id,
                    project_id,
                    host,
                    repository,
                    number,
                }),
                None,
            ),
            Intent::SetPullRequestWatch {
                thread_id,
                project_id,
                host,
                repository,
                number,
                url,
                enabled,
            } => Next::call(
                Call::SetPullRequestWatch(pr::SetPullRequestWatch {
                    thread_id,
                    project_id,
                    link: agent_domain::PullRequestLink {
                        host,
                        repository,
                        number,
                        url,
                        source: agent_domain::PullRequestLinkSource::Manual,
                        linked_at: agent_domain::Timestamp::from_millis(super::owner::now_ms() as i64)
                            .map_err(|error| invalid(error))?,
                        snapshot: None,
                        stack: None,
                        watch: None,
                    },
                    enabled,
                }),
                None,
            ),
            Intent::LoadSourceControlAuth { cwd } => Next::call(
                Call::SourceControlAuth(pr::SourceControlAuthRequest {
                    host: None,
                    cwd,
                    fresh: true,
                }),
                None,
            ),
            Intent::LoadSourceControlDiscovery { cwd } => Next::call(
                Call::SourceControlDiscovery(pr::SourceControlDiscoveryRequest {
                    cwd,
                    fresh: true,
                }),
                None,
            ),
            Intent::ReorderPinned {
                thread_id: moved,
                before_thread_id,
            } => self.reorder_pinned(moved, before_thread_id)?,
            Intent::EditDraft { text, base_text } => {
                self.ensure_new_thread_draft();
                let mut draft = self.state.current_draft();
                draft.text = match base_text {
                    Some(base) => merge_draft_text(base, text, draft.text.clone()),
                    None => text,
                };
                let key = self.state.draft_key();
                self.state.drafts.insert(key, draft);
                Next::Done
            }
            Intent::AttachFiles { draft_key, files } => {
                self.attach_files(draft_key, files)?;
                Next::Done
            }
            Intent::SelectComposerItem {
                text,
                cursor,
                item_id,
            } => self.select_composer_item(text, cursor, item_id)?,
            Intent::RemoveDraftContext { context_id } => {
                self.remove_draft_context(&context_id);
                Next::Done
            }
            Intent::AddTerminalContext {
                text,
                cursor,
                selection,
            } => self.add_terminal_context(text, cursor, &selection),
            Intent::AddThreadContexts {
                text,
                cursor,
                thread_ids,
            } => self.add_thread_contexts(text, cursor, &thread_ids),
            Intent::AttachTerminalOutput {
                thread_id: id,
                output,
            } => self.attach_terminal_output(thread_id(id)?, &output)?,
            Intent::DiscardDraft { draft_key } => {
                self.discard_draft(draft_key);
                Next::Done
            }
            Intent::UndoThreadAction => self.undo_thread_action()?,
            Intent::SelectTrait {
                descriptor_id,
                choice,
            } => self.select_trait(&descriptor_id, &choice)?,
            Intent::ToggleTrait { descriptor_id, on } => self.toggle_trait(&descriptor_id, on)?,
            Intent::StashDraft => self.stash_draft()?,
            Intent::FinalizeStashImages { entry_id, images } => {
                self.finalize_stash_images(&entry_id, images);
                Next::Done
            }
            Intent::RestoreStash { entry_id } => self.restore_stash(&entry_id)?,
            Intent::DeleteStash { entry_id } => {
                self.state.stash.take(&entry_id);
                Next::Done
            }
            Intent::EditAnswer {
                request_id,
                question_id,
                edit,
            } => self.edit_answer(request_id, &question_id, edit)?,
            Intent::ShowQuestion { request_id, index } => {
                self.state
                    .question_drafts
                    .entry(request_id)
                    .or_default()
                    .question_index = index;
                Next::Done
            }
            Intent::SubmitAnswers { request_id } => self.submit_answers(request_id)?,
            Intent::MoveThread {
                thread_id: moved,
                section,
                destination,
            } => self.move_thread(&moved, section, &destination)?,
            Intent::DropThread {
                thread_id: id,
                plan,
            } => self.drop_thread(thread_id(id)?, plan)?,
            Intent::LimitRecovery {
                thread_id: id,
                action,
            } => self.limit_recovery(thread_id(id)?, action)?,
            Intent::DismissThreadError { dismiss_key } => {
                self.state.error_dismissals.dismiss(Some(&dismiss_key));
                self.state.error = None;
                Next::Done
            }
            Intent::SelectDiffScope { choice } => self.select_diff_scope(&choice)?,
            Intent::SelectDiffTurn { run_id, file_path } => {
                self.select_diff_turn(&run_id, file_path.as_deref())?
            }
            Intent::SelectDiffBaseRef { base_ref } => {
                self.select_diff_base_ref(base_ref.as_deref())?
            }
            Intent::SetDiffIgnoreWhitespace { ignore } => {
                self.state.preferences.diff_ignore_whitespace = ignore;
                if self.state.selected_thread.is_some() {
                    self.load_diff()?
                } else {
                    Next::Done
                }
            }
            Intent::LoadDiff => self.load_diff()?,
            Intent::LoadMoreDiffFiles => {
                self.want_diff_files(crate::view::review_files::FileRequest::Next);
                Next::Done
            }
            Intent::RevealDiffFile { path, retry } => {
                self.want_diff_files(crate::view::review_files::FileRequest::Reveal(&path));
                if retry {
                    self.retry_diff_file(&path);
                }
                Next::Done
            }
            Intent::OpenTerminal {
                thread_id: id,
                terminal_id,
                cols,
                rows,
            } => self.open_terminal(
                thread_id(id)?,
                terminal_id,
                None,
                op::TerminalSize { cols, rows },
                vec![],
            )?,
            Intent::NewTerminal {
                thread_id: id,
                cols,
                rows,
            } => self.new_terminal(
                thread_id(id)?,
                None,
                op::TerminalSize { cols, rows },
                vec![],
            )?,
            Intent::SplitTerminal {
                thread_id: id,
                terminal_id,
                cols,
                rows,
            } => self.new_terminal(
                thread_id(id)?,
                Some(&terminal_id),
                op::TerminalSize { cols, rows },
                vec![],
            )?,
            Intent::RunProjectScript {
                thread_id: id,
                script_id,
                cols,
                rows,
            } => self.run_project_script(
                thread_id(id)?,
                &script_id,
                op::TerminalSize { cols, rows },
            )?,
            Intent::WriteTerminal {
                thread_id: id,
                terminal_id,
                data,
            } => self.terminal_call(&thread_id(id)?, &terminal_id, |handle| {
                Call::WriteTerminal(op::TerminalWrite {
                    process_handle: handle,
                    data,
                })
            }),
            Intent::ResizeTerminal {
                thread_id: id,
                terminal_id,
                cols,
                rows,
            } => self.terminal_call(&thread_id(id)?, &terminal_id, |handle| {
                Call::ResizeTerminal(op::ResizeTerminal {
                    handle,
                    size: op::TerminalSize { cols, rows },
                })
            }),
            Intent::DetachTerminal {
                thread_id: id,
                terminal_id,
            } => self.terminal_call(&thread_id(id)?, &terminal_id, |handle| {
                Call::DetachTerminal(op::DetachTerminal { handle })
            }),
            Intent::CloseTerminal {
                thread_id: id,
                terminal_id,
            } => self.close_terminal(&thread_id(id)?, &terminal_id),
            Intent::UpdateComposerMenu {
                text,
                cursor,
                layout,
            } => {
                self.update_composer_menu(&text, cursor, layout);
                Next::Done
            }
            Intent::SearchDiffBaseRefs { query } => {
                let cwd = self.state.cwd();
                self.load_refs(cwd.clone(), crate::state::RefScope::Local, query.clone());
                self.load_refs(cwd, crate::state::RefScope::Remote, query);
                Next::Done
            }
            Intent::SearchNewThreadBranches { query } => {
                self.load_new_thread_branches(query);
                Next::Done
            }
            Intent::LoadMoreNewThreadBranches => {
                self.load_more_new_thread_branches();
                Next::Done
            }
            Intent::SearchScheduledTaskBranches { project_id, query } => {
                self.load_scheduled_task_branches(project_id, query);
                Next::Done
            }
            Intent::LoadMoreScheduledTaskBranches { project_id } => {
                self.load_more_scheduled_task_branches(project_id);
                Next::Done
            }
            Intent::SetNewThreadWorkspace { mode } => self.set_new_thread_workspace(mode)?,
            Intent::SelectNewThreadBranch {
                branch,
                worktree_path,
            } => self.select_new_thread_branch(branch, worktree_path)?,
            Intent::CreateNewThreadBranch { name } => self.create_new_thread_branch(name)?,
            Intent::SetNewThreadStartFromOrigin { on } => {
                self.set_new_thread_start_from_origin(on)?
            }
            Intent::NewThreadOnBranch {
                project_id,
                branch,
                worktree_path,
            } => {
                self.select_thread(None);
                self.state.begin_new_thread_draft(
                    new_id("new"),
                    Some(project_id),
                    super::owner::now_ms() as i64,
                );
                self.start_new_thread_on_branch(branch, worktree_path);
                self.load_new_thread_branches(String::new());
                Next::Done
            }
            Intent::RetryPreparation { run_id: run } => {
                let thread = self.selected()?;
                Next::Commands(vec![
                    self.command(thread, Command::RetryPrepared { run: run_id(run)? }),
                ])
            }
            Intent::WorkLocally => self.work_locally()?,
            Intent::CompactContext => self.compact_context()?,
            Intent::SetProjectIcon { project_id, path } => self.set_project_icon(project_id, path),
            Intent::ClearTerminal {
                thread_id: id,
                terminal_id,
            } => self.clear_terminal(thread_id(id)?, terminal_id),
            Intent::RestartTerminal {
                thread_id: id,
                terminal_id,
                cols,
                rows,
            } => {
                self.restart_terminal(thread_id(id)?, terminal_id, op::TerminalSize { cols, rows })?
            }
            Intent::SetTerminalFontSize { size } => {
                self.state.preferences.terminal_font_size = Some(
                    crate::view::terminals::text_size::normalize_terminal_font_size(Some(size)),
                );
                Next::Done
            }
            Intent::SetFollowUpBehavior { behavior } => {
                self.state.follow_up = behavior;
                Next::Done
            }
            Intent::SetTimestampFormat { format } => {
                self.state.preferences.timestamp_format = format;
                Next::Done
            }
            Intent::SetWorkingSection { enabled } => {
                self.state.preferences.working_section = enabled;
                Next::Done
            }
            Intent::SetNotificationMode { mode } => {
                self.state.preferences.notification_mode = mode;
                Next::Done
            }
            Intent::SetInAppNotificationsEnabled { enabled } => {
                self.state.preferences.in_app_notifications_enabled = enabled;
                Next::Done
            }
            Intent::SetLoadBalancingEnabled { enabled } => {
                self.state.preferences.load_balancing_enabled = enabled;
                Next::Done
            }
            Intent::SetLoadBalancingWeight {
                instance_id,
                weight,
            } => {
                if weight > 100 {
                    return Err(invalid("Load balancing weights must be 0 to 100."));
                }
                self.state
                    .preferences
                    .load_balancing_weights
                    .insert(instance_id, weight);
                Next::Done
            }
            Intent::SetSnapshotCaptureEnabled { enabled } => {
                self.state.preferences.snapshot_capture.enabled = enabled;
                Next::Done
            }
            Intent::SetSnapshotIncludeAccessibility { enabled } => {
                self.state
                    .preferences
                    .snapshot_capture
                    .include_accessibility = enabled;
                Next::Done
            }
            Intent::SetSnapshotShortcut { shortcut } => {
                self.state.preferences.snapshot_capture.shortcut = shortcut;
                Next::Done
            }
            Intent::SetSnapshotPlaySound { enabled } => {
                self.state.preferences.snapshot_capture.play_sound = enabled;
                Next::Done
            }
            Intent::SetSnapshotSound { sound } => {
                self.state.preferences.snapshot_capture.sound = sound;
                Next::Done
            }
            Intent::SetSnapshotFlash { enabled } => {
                self.state.preferences.snapshot_capture.flash = enabled;
                Next::Done
            }
            Intent::SetSnapshotAnimations { enabled } => {
                self.state.preferences.snapshot_capture.animations = enabled;
                Next::Done
            }
            Intent::ImportShare { content } => {
                let incoming = crate::view::share::compose(&content);
                if !incoming.is_empty() {
                    let key = self.state.draft_key();
                    let mut draft = self.state.current_draft();
                    if !draft.text.is_empty() {
                        draft.text.push_str("\n\n");
                    }
                    draft.text.push_str(&incoming);
                    self.state.drafts.insert(key, draft);
                }
                Next::Done
            }
            Intent::SetDefaultModel {
                instance_id,
                driver,
                model,
                options,
            } => {
                let mut defaults = self.state.default_draft.user_defaults();
                defaults.instance_id = instance_id;
                defaults.driver = driver;
                defaults.model = model;
                defaults.options = options;
                let selection = defaults.selection().map_err(invalid)?;
                self.state.default_draft = defaults;
                Next::call(
                    Call::UpdateSettings(Box::new(m::HostSettingsPatch {
                        default_model_selection: Some(m::Nullable::Value(selection)),
                        ..Default::default()
                    })),
                    None,
                )
            }
            Intent::SetDefaultRuntimeMode { mode } => {
                let mut defaults = self.state.default_draft.user_defaults();
                defaults.runtime_mode = mode;
                self.state.default_draft = defaults;
                Next::Done
            }
            Intent::ToggleFavoriteModel { instance_id, model } => {
                self.toggle_favorite_model(&instance_id, &model);
                Next::Done
            }
            Intent::SetModelOrder {
                instance_id,
                models,
            } => {
                self.state
                    .preferences
                    .model_order
                    .insert(instance_id, models);
                Next::Done
            }
            Intent::DismissResumeCompaction { key } => {
                self.state.resume_compaction_dismissals.insert(key);
                Next::Done
            }
            Intent::UpsertKeybinding { rule, replace } => Next::call(
                Call::UpsertKeybinding(agent_protocol::keybindings::UpsertKeybinding {
                    rule: rule.into(),
                    replace: replace.map(Into::into),
                }),
                None,
            ),
            Intent::RemoveKeybinding { rule } => {
                Next::call(Call::RemoveKeybinding(rule.into()), None)
            }
            Intent::LoadSettings => Next::call(Call::ReadSettings(m::Empty {}), None),
            Intent::UpdateSettings { scope, change } => {
                match plan_settings_update(&scope, &change) {
                    Some(patch) => Next::call(Call::UpdateSettings(Box::new(patch)), None),
                    None => Next::Done,
                }
            }
            Intent::LoadBackgroundPolicy => {
                Next::call(Call::ReadBackground(agent_protocol::background::ReadBackground {}), None)
            }
            Intent::LoadDiagnostics { trace_file_path } => {
                self.job(Call::ReadBackground(agent_protocol::background::ReadBackground {}), None, None);
                self.job(Call::ReadHostResources(agent_protocol::background::ReadHostResources {}), None, None);
                self.job(Call::ReadProcessDiagnostics(agent_protocol::background::ReadProcessDiagnostics {}), None, None);
                self.job(
                    Call::ReadProcessResourceHistory(
                        agent_protocol::background::ReadProcessResourceHistory {
                            window_ms: 60 * 60_000,
                            bucket_ms: 60_000,
                        },
                    ),
                    None,
                    None,
                );
                self.job(
                    Call::ReadTraceDiagnostics(agent_protocol::background::ReadTraceDiagnostics {
                        trace_file_path,
                        max_files: 16,
                        slow_span_threshold_ms: Some(1_000.0),
                    }),
                    None,
                    None,
                );
                Next::Done
            }
            Intent::SetBackgroundProfile { profile } => {
                if self.state.background_policy.is_none() {
                    return Next::Done;
                }
                let profile = match profile.as_str() {
                    "balanced" => agent_domain::BackgroundActivityProfile::Balanced,
                    "performance" => agent_domain::BackgroundActivityProfile::Performance,
                    "battery-saver" => agent_domain::BackgroundActivityProfile::BatterySaver,
                    _ => return Next::Done,
                };
                let policy = agent_domain::BackgroundActivityPolicy::preset(profile);
                Next::call(
                    Call::UpdateBackgroundPolicy(
                        agent_protocol::background::UpdateBackgroundPolicy { policy },
                    ),
                    None,
                )
            }
            Intent::SetAutomaticGitFetchInterval { seconds } => {
                let Some(current) = self.state.background_policy.as_ref() else {
                    return Next::Done;
                };
                let mut policy = current.policy.clone();
                policy.automatic_git_fetch_interval_ms = u64::from(seconds).saturating_mul(1_000);
                Next::call(
                    Call::UpdateBackgroundPolicy(
                        agent_protocol::background::UpdateBackgroundPolicy { policy },
                    ),
                    None,
                )
            }
            Intent::SetProviderHealthRefreshInterval { seconds } => {
                let Some(current) = self.state.background_policy.as_ref() else {
                    return Next::Done;
                };
                let mut policy = current.policy.clone();
                policy.provider_health_refresh_interval_ms =
                    u64::from(seconds).saturating_mul(1_000);
                Next::call(
                    Call::UpdateBackgroundPolicy(
                        agent_protocol::background::UpdateBackgroundPolicy { policy },
                    ),
                    None,
                )
            }
            Intent::ResetProjectSettings { project_id } => Next::call(
                Call::UpdateSettings(Box::new(clear_project_overrides(
                    &project_id,
                    &[
                        ProjectSettingKey::AutoSettle,
                        ProjectSettingKey::ContinueAfterRestart,
                        ProjectSettingKey::DefaultRuntimeMode,
                        ProjectSettingKey::DefaultThreadEnvMode,
                        ProjectSettingKey::WorktreeSubmodules,
                        ProjectSettingKey::NewWorktreesStartFromOrigin,
                        ProjectSettingKey::AgentBrowserAccess,
                        ProjectSettingKey::DefaultAutoPull,
                        ProjectSettingKey::AutoSettleOnMerge,
                        ProjectSettingKey::ResponseStreamingMode,
                        ProjectSettingKey::BranchNamingMode,
                        ProjectSettingKey::PullRequestMergeMethod,
                    ],
                ))),
                None,
            ),
            Intent::UpdateProjectScripts {
                project_id,
                scripts,
            } => self.update_project_scripts(project_id, scripts),
            Intent::ScanSessions => self.scan_sessions(),
            Intent::SelectImportSessions { paths, checked } => {
                self.select_import_sessions(&paths, checked);
                Next::Done
            }
            Intent::CloseImport => {
                self.state.session_import = SessionImport::default();
                Next::Done
            }
            Intent::RetryAttachment { draft_key, id } => {
                let key = draft_key.unwrap_or_else(|| self.state.draft_key());
                self.begin_attachment(key, id)?;
                Next::Done
            }
            Intent::RemoveAttachment { draft_key, id } => {
                let key = draft_key.unwrap_or_else(|| self.state.draft_key());
                if let Some(draft) = self.state.drafts.get_mut(&key) {
                    draft.attachments.retain(|a| a.id != id);
                }
                Next::Done
            }
            Intent::Send { alternate } => self.send_draft(alternate)?,
            Intent::Stop => {
                let thread = self.selected()?;
                let command = self
                    .state
                    .thread_state(&thread)
                    .and_then(|state| interrupt_command(state, None))
                    .ok_or_else(|| invalid("No active work"))?;
                Next::Commands(vec![self.command(thread, command)])
            }
            Intent::StopSessions => {
                let thread = self.selected()?;
                let base = self.new_command_id();
                let commands = self
                    .state
                    .thread_state(&thread)
                    .map(|state| detach_commands(state, &base))
                    .unwrap_or_default();
                Next::Commands(
                    commands
                        .into_iter()
                        .map(|(id, command)| {
                            PendingCommand::new(
                                thread.clone(),
                                Request::Dispatch(Box::new(dispatch(thread.clone(), id, command))),
                                self.now(),
                            )
                        })
                        .collect(),
                )
            }
            Intent::DiscardPending { command_id } => {
                self.discard(&agent_domain::CommandId::new(command_id).map_err(invalid)?);
                Next::Done
            }
            Intent::Fork {
                source_thread_id,
                run_id: run,
            } => {
                let source = thread_id(source_thread_id)?;
                if self.state.context_pending(&source) {
                    return Ok(Next::Done);
                }
                let target = thread_id(new_id("thread"))?;
                let command =
                    fork_command(target, run_id(run)?, None, &self.options.creation_source);
                let mut entry = self.command(source, command);
                entry.navigate = true;
                Next::Commands(vec![entry])
            }
            Intent::MergeBack => {
                let thread = self.selected()?;
                if self.state.context_pending(&thread) {
                    return Ok(Next::Done);
                }
                let state = self
                    .state
                    .thread_state(&thread)
                    .ok_or_else(|| invalid("Thread is loading"))?;
                let run = latest_merge_back_run(state)
                    .ok_or_else(|| invalid("Wait for the latest run to finish"))?
                    .id
                    .clone();
                let parent = state
                    .thread
                    .as_ref()
                    .and_then(|t| t.parent.clone())
                    .ok_or_else(|| invalid("Thread is not a fork"))?;
                let mut entry = self.command(thread, merge_back_command(parent, run));
                entry.navigate = true;
                Next::Commands(vec![entry])
            }
            Intent::PlanFollowUp { new_thread } => self.plan_follow_up(new_thread)?,
            Intent::Rollback {
                checkpoint_id,
                restore_files,
            } => {
                let thread = self.selected()?;
                let checkpoint = self
                    .state
                    .thread_state(&thread)
                    .and_then(|state| {
                        state
                            .checkpoints
                            .iter()
                            .find(|c| c.id.as_str() == checkpoint_id)
                    })
                    .ok_or_else(|| invalid("Checkpoint unavailable"))?;
                let command = rollback_command(checkpoint, restore_files);
                Next::Commands(vec![self.command(thread, command)])
            }
            Intent::Thread {
                thread_id: id,
                action,
            } => {
                let action = match action {
                    ThreadAction::Pin => LifecycleAction::Pin { order: None },
                    ThreadAction::Unpin => LifecycleAction::Unpin,
                    ThreadAction::Settle => LifecycleAction::Settle,
                    ThreadAction::Unsettle => LifecycleAction::Unsettle,
                    ThreadAction::Snooze { until } => LifecycleAction::Snooze {
                        until: Timestamp::parse(&until).map_err(invalid)?,
                    },
                    ThreadAction::Unsnooze => LifecycleAction::Unsnooze,
                    ThreadAction::Rename { title } => LifecycleAction::Rename { title },
                    ThreadAction::RegenerateTitle => LifecycleAction::RegenerateTitle,
                    ThreadAction::MarkUnread => LifecycleAction::MarkUnread,
                    ThreadAction::AutoSettle { enabled } => LifecycleAction::AutoSettle { enabled },
                    ThreadAction::Archive => LifecycleAction::Archive,
                    ThreadAction::Unarchive => LifecycleAction::Unarchive,
                    ThreadAction::Delete => LifecycleAction::Delete,
                    ThreadAction::PinReorder { order_key } => {
                        LifecycleAction::ReorderPinned { order: order_key }
                    }
                    ThreadAction::ActiveReorder { order_key } => {
                        LifecycleAction::ReorderActive { order: order_key }
                    }
                    ThreadAction::Visit { at } => LifecycleAction::Visit {
                        at: Timestamp::from_millis(at).map_err(invalid)?,
                    },
                };
                Next::Commands(vec![self.lifecycle(thread_id(id)?, action)])
            }
            Intent::Queue { action } => self.queue(action)?,
            Intent::SetModel {
                instance_id,
                driver,
                model,
                options,
            } => {
                self.ensure_new_thread_draft();
                let mut draft = self.state.current_draft();
                draft.instance_id = instance_id;
                draft.driver = driver;
                draft.model = model;
                draft.options = options;
                let selection = draft.selection().map_err(invalid)?;
                self.state.default_draft = draft.user_defaults();
                let key = self.state.draft_key();
                self.state.drafts.insert(key, draft);
                self.thread_command(select_model_command(selection))
            }
            Intent::SaveStagedModel { staged } => self.prepare(Intent::SetModel {
                instance_id: staged.instance_id,
                driver: staged.driver,
                model: staged.model,
                options: staged.options,
            })?,
            Intent::RememberModelOptions {
                instance_id,
                model,
                options,
            } => {
                remember_model_options(
                    &mut self.state.preferences.model_options,
                    &instance_id,
                    &model,
                    &options,
                );
                Next::Done
            }
            Intent::SetRuntimeMode { mode } => {
                self.update_draft(|draft| draft.runtime_mode = mode);
                self.thread_command(Command::RuntimeMode { mode })
            }
            Intent::SetInteractionMode { mode } => {
                self.update_draft(|draft| draft.interaction_mode = mode);
                self.thread_command(Command::InteractionMode { mode })
            }
            Intent::RespondApproval {
                request_id,
                decision,
            } => {
                let thread = self.selected()?;
                let request = RuntimeRequestId::new(request_id).map_err(invalid)?;
                let command = approval_command(request, approval_decision(&decision)?);
                Next::Commands(vec![self.command(thread, command)])
            }
            Intent::DismissInput { request_id } => {
                let thread = self.selected()?;
                let command = Command::DismissQuestion {
                    request: RuntimeRequestId::new(request_id).map_err(invalid)?,
                };
                Next::Commands(vec![self.command(thread, command)])
            }
            Intent::LoadEarlier => {
                self.load_earlier()?;
                Next::Done
            }
            Intent::LoadItemDetail { item_id } => {
                self.load_detail(agent_domain::TurnItemId::new(item_id).map_err(invalid)?)?;
                Next::Done
            }
            Intent::CancelSetup => Next::call(
                Call::CancelSetup(c::CancelSetup {
                    thread_id: self.selected()?,
                }),
                None,
            ),
            Intent::Refresh => {
                self.refresh();
                self.app_became_active();
                Next::Done
            }
            Intent::PreviewSelectTab { tab_id } => {
                if self.state.preview.session(&tab_id).is_none() {
                    return Err(invalid("Preview tab is unavailable"));
                }
                self.state.preview.active_tab = Some(tab_id);
                Next::Done
            }
            other => self.peripheral(other)?,
        })
    }

    fn update_draft(&mut self, change: impl FnOnce(&mut Draft)) {
        self.ensure_new_thread_draft();
        let mut draft = self.state.current_draft();
        change(&mut draft);
        let key = self.state.draft_key();
        self.state.drafts.insert(key, draft);
    }

    /// A thread setting changes the open thread; a new-thread draft keeps it.
    fn thread_command(&self, command: Command) -> Next {
        match &self.state.selected_thread {
            Some(thread) => Next::Commands(vec![self.command(thread.clone(), command)]),
            None => Next::Done,
        }
    }

    fn reorder_pinned(&mut self, moved: String, before: Option<String>) -> Result<Next, PeerError> {
        let shell = self
            .state
            .shell_view()
            .ok_or_else(|| invalid("Pinned thread is unavailable"))?
            .into_owned();
        let mut pinned: Vec<_> = shell
            .threads
            .iter()
            .filter(|row| row.pinned_at.is_some())
            .collect();
        sort_pinned_by_order(&mut pinned);
        let mut ids: Vec<String> = pinned.iter().map(|row| row.id.to_string()).collect();
        let from = ids
            .iter()
            .position(|id| id == &moved)
            .ok_or_else(|| invalid("Pinned thread is unavailable"))?;
        if before.as_ref() == Some(&moved) {
            return Ok(Next::Done);
        }
        ids.remove(from);
        let to = match before {
            Some(before) => ids
                .iter()
                .position(|id| id == &before)
                .ok_or_else(|| invalid("Pinned list changed; try again"))?,
            None => ids.len(),
        };
        ids.insert(to, moved.clone());
        let keys = pinned
            .iter()
            .map(|row| (row.id.to_string(), row.pin_order.clone()))
            .collect();
        let entries = crate::ordering::reorder(&ids, &keys, &moved)
            .into_iter()
            .map(|(id, order)| {
                Ok(self.lifecycle(thread_id(id)?, LifecycleAction::ReorderPinned { order }))
            })
            .collect::<Result<_, PeerError>>()?;
        Ok(Next::Commands(entries))
    }

    fn send_draft(&mut self, alternate: bool) -> Result<Next, PeerError> {
        if self.state.editing_run.is_some() {
            return self.queue(QueueAction::SaveEdit);
        }
        self.ensure_new_thread_draft();
        let draft = self.state.current_draft();
        let slash = draft.text.trim().to_ascii_lowercase();
        if matches!(slash.as_str(), "/plan" | "/default") && draft.attachments.is_empty() {
            let mode = if slash == "/plan" {
                InteractionMode::Plan
            } else {
                InteractionMode::Default
            };
            self.update_draft(|draft| {
                draft.text.clear();
                draft.interaction_mode = mode;
            });
            return Ok(self.thread_command(Command::InteractionMode { mode }));
        }
        if self.options.creation_source == "desktop"
            && !alternate
            && actionable_plan(&self.state).is_some()
        {
            return self.plan_follow_up(false);
        }
        if draft.is_empty() {
            return Err(invalid("Enter a message"));
        }
        let selection = draft.selection().map_err(invalid)?;
        let attachments = draft.attachment_refs().map_err(invalid)?;
        let names: Vec<&str> = attachments.iter().map(|a| a.name.as_str()).collect();
        let title_seed = thread_title_seed(&draft.text, &names, &[]);
        let message = TurnMessage {
            id: MessageId::new(new_id("message")).map_err(invalid)?,
            text: draft.text.clone(),
            attachments: attachments.clone(),
            context: draft.context.clone(),
        };
        let key = self.state.draft_key();
        let restore = Restore {
            draft_key: key.clone(),
            text: draft.text.clone(),
            attachments,
            context: draft.context.clone(),
        };
        let mut entry = match self.state.selected_thread.clone() {
            Some(thread) => {
                let running = self
                    .state
                    .thread_state(&thread)
                    .is_some_and(|state| state.active_run().is_some());
                let mode =
                    resolve_composer_dispatch_mode(running, alternate, Some(self.state.follow_up));
                let command = send_command(StartTurn {
                    message,
                    selection: Some(selection),
                    title_seed: Some(title_seed),
                    source_plan: None,
                    dispatch: mode.into(),
                    continuation: None,
                    creation_source: self.options.creation_source.clone(),
                });
                self.command(thread, command)
            }
            None => {
                let workspace = crate::view::new_thread::new_thread_launch_workspace(&self.state)
                    .map_err(invalid)?;
                let thread = thread_id(new_id("thread"))?;
                let launch = launch(LaunchThread {
                    command_id: self.new_command_id(),
                    thread: Some(thread.clone()),
                    project: self
                        .state
                        .new_thread_project_id()
                        .map(str::to_owned)
                        .unwrap_or_else(|| CHATS_PROJECT.into()),
                    title: title_seed.clone(),
                    title_seed: Some(title_seed),
                    selection,
                    runtime_mode: draft.runtime_mode,
                    interaction_mode: draft.interaction_mode,
                    workspace,
                    message: Some(message),
                    creation_source: self.options.creation_source.clone(),
                });
                let mut entry =
                    PendingCommand::new(thread, Request::Launch(Box::new(launch)), self.now());
                entry.navigate = true;
                entry
            }
        };
        entry.restore = Some(restore);
        let mut cleared = draft;
        cleared.text.clear();
        cleared.attachments.clear();
        cleared.context = None;
        self.state.drafts.insert(key, cleared);
        Ok(Next::Commands(vec![entry]))
    }

    fn compact_context(&mut self) -> Result<Next, PeerError> {
        let thread = self.selected()?;
        let selection = self.state.current_draft().selection().map_err(invalid)?;
        let command = send_command(StartTurn {
            message: TurnMessage {
                id: MessageId::new(new_id("message")).map_err(invalid)?,
                text: "/compact".into(),
                attachments: vec![],
                context: None,
            },
            selection: Some(selection),
            title_seed: None,
            source_plan: None,
            dispatch: resolve_composer_dispatch_mode(false, false, Some(self.state.follow_up))
                .into(),
            continuation: None,
            creation_source: self.options.creation_source.clone(),
        });
        Ok(Next::Commands(vec![self.command(thread, command)]))
    }

    fn plan_follow_up(&mut self, new_thread: bool) -> Result<Next, PeerError> {
        let thread = self.selected()?;
        let state = self
            .state
            .thread_state(&thread)
            .ok_or_else(|| invalid("Thread not loaded"))?;
        let plan = actionable_plan(&self.state).ok_or_else(|| invalid("No actionable plan"))?;
        let markdown = crate::view::plan::proposed_plan_markdown(state, plan);
        let plan_ref = PlanRef {
            thread: thread.clone(),
            plan: plan.id.clone(),
        };
        let current = state.thread.as_ref().expect("thread record");
        let (project, workspace, mode_now) = (
            current.project.clone(),
            current.workspace.clone(),
            current.interaction_mode,
        );
        let draft = self.state.current_draft();
        let selection = draft.selection().map_err(invalid)?;
        let message = |text: String| -> Result<TurnMessage, PeerError> {
            Ok(TurnMessage {
                id: MessageId::new(new_id("message")).map_err(invalid)?,
                text,
                attachments: vec![],
                context: None,
            })
        };
        if new_thread {
            let child = thread_id(new_id("thread"))?;
            let launch = launch(LaunchThread {
                command_id: self.new_command_id(),
                thread: Some(child.clone()),
                project,
                title: plan_implementation_thread_title(&markdown),
                title_seed: None,
                selection,
                runtime_mode: self.state.default_draft.runtime_mode,
                interaction_mode: InteractionMode::Default,
                workspace: WorkspaceChoice::Local {
                    branch: workspace.as_ref().and_then(|w| w.branch.clone()),
                    worktree_path: workspace.and_then(|w| w.worktree_path),
                },
                message: Some(message(plan_implementation_prompt(&markdown))?),
                creation_source: self.options.creation_source.clone(),
            });
            let mut entry =
                PendingCommand::new(child, Request::Launch(Box::new(launch)), self.now());
            entry.navigate = true;
            return Ok(Next::Commands(vec![entry]));
        }
        let (text, mode) = plan_follow_up(&draft.text, &markdown);
        let implementing = mode == InteractionMode::Default;
        let mut entries = vec![];
        if mode != mode_now {
            entries.push(self.command(thread.clone(), Command::InteractionMode { mode }));
            self.update_draft(|draft| draft.interaction_mode = mode);
        }
        let mut send = self.command(
            thread.clone(),
            send_command(StartTurn {
                message: message(text)?,
                selection: Some(selection),
                title_seed: None,
                source_plan: implementing.then_some(plan_ref),
                dispatch: TurnDispatch::Auto,
                continuation: None,
                creation_source: self.options.creation_source.clone(),
            }),
        );
        if !implementing {
            let key = self.state.draft_key();
            send.restore = Some(Restore {
                draft_key: key.clone(),
                text: draft.text.clone(),
                attachments: vec![],
                context: None,
            });
            self.update_draft(|draft| draft.text.clear());
        }
        entries.push(send);
        Ok(Next::Commands(entries))
    }

    fn queue(&mut self, action: QueueAction) -> Result<Next, PeerError> {
        let thread = self.selected()?;
        Ok(match action {
            QueueAction::Resume => Next::Commands(vec![self.command(thread, Command::ResumeQueue)]),
            QueueAction::Cancel { run_id: run } => Next::Commands(vec![
                self.command(thread, Command::CancelQueued { run: run_id(run)? }),
            ]),
            QueueAction::Steer { run_id: run } => {
                let active = self
                    .state
                    .thread_state(&thread)
                    .and_then(State::active_run)
                    .map(|run| run.id.clone())
                    .ok_or_else(|| invalid("No active run"))?;
                Next::Commands(vec![self.command(
                    thread,
                    Command::PromoteToSteer {
                        queued: run_id(run)?,
                        active,
                    },
                )])
            }
            QueueAction::Edit { run_id: run } => {
                let run = run_id(run)?;
                let queued = self
                    .state
                    .thread_state(&thread)
                    .map(queue_workflow)
                    .and_then(|workflow| workflow.queued.into_iter().find(|q| q.run == run))
                    .ok_or_else(|| invalid("Queued run is unavailable"))?;
                let mut draft = self.state.current_draft();
                draft.text = queued.text;
                draft.attachments = queued
                    .attachments
                    .iter()
                    .map(DraftAttachment::from_remote)
                    .collect();
                draft.context = queued.context;
                self.state.editing_run = Some(run);
                let key = self.state.draft_key();
                self.state.drafts.insert(key, draft);
                Next::Done
            }
            QueueAction::SaveEdit => {
                let run = self
                    .state
                    .editing_run
                    .clone()
                    .ok_or_else(|| invalid("No queued message is being edited"))?;
                let draft = self.state.current_draft();
                let attachments = draft.attachment_refs().map_err(invalid)?;
                let mut entry = self.command(
                    thread.clone(),
                    Command::EditQueued {
                        run,
                        text: draft.text.clone(),
                        attachments: Some(attachments.clone()),
                        context: draft.context.clone(),
                    },
                );
                entry.restore = Some(Restore {
                    draft_key: thread.to_string(),
                    text: draft.text.clone(),
                    attachments,
                    context: draft.context.clone(),
                });
                let key = self.state.draft_key();
                self.state.drafts.remove(&key);
                self.state.editing_run = None;
                Next::Commands(vec![entry])
            }
            QueueAction::CancelEdit => {
                let key = self.state.draft_key();
                self.state.drafts.remove(&key);
                self.state.editing_run = None;
                Next::Done
            }
            QueueAction::Move {
                run_id: run,
                before_run_id,
            } => Next::Commands(vec![self.command(
                thread,
                Command::ReorderQueued {
                    run: run_id(run)?,
                    before: before_run_id.map(run_id).transpose()?,
                },
            )]),
        })
    }

    /// Admits the picked files by the reference rules, then uploads the accepted ones.
    /// Images over the size limit must be downscaled by the client first.
    fn attach_files(&mut self, mut key: String, files: Vec<LocalFile>) -> Result<(), PeerError> {
        let answer_draft = key.starts_with("answer:");
        if self.state.selected_thread.is_none() && !answer_draft {
            let was_current = key == self.state.draft_key();
            self.ensure_new_thread_draft();
            if was_current && !self.state.drafts.contains_key(&key) {
                key = self.state.draft_key();
            }
        }
        if key != self.state.draft_key()
            && !self.state.drafts.contains_key(&key)
            && !answer_draft
        {
            return Err(invalid("The attachment draft is no longer available"));
        }
        let mut draft = self.state.drafts.get(&key).cloned().unwrap_or_else(|| {
            if answer_draft {
                Draft::default()
            } else {
                self.state.current_draft()
            }
        });
        let mut candidates = vec![];
        let mut readable = vec![];
        let mut error = None;
        for file in files {
            match std::fs::metadata(&file.path) {
                Ok(metadata) if metadata.is_file() => {
                    candidates.push(AttachmentCandidate {
                        name: file.name.clone(),
                        mime_type: file.mime_type.to_ascii_lowercase(),
                        size_bytes: metadata.len(),
                    });
                    readable.push(file);
                }
                _ => error = Some(format!("'{}' is empty or could not be read.", file.name)),
            }
        }
        let admission = admit_attachments(&draft.attachments, &candidates);
        let mut added = vec![];
        for admitted in admission.accepted {
            let index = admitted.index as usize;
            if admitted.needs_compression {
                error = Some(image_preparation_error(&admitted.name, false));
                continue;
            }
            let id = new_id("attachment");
            draft.attachments.push(DraftAttachment {
                id: id.clone(),
                remote_id: None,
                name: admitted.name,
                kind: match admitted.kind {
                    AttachmentFileKind::Image => "image",
                    _ => "file",
                }
                .into(),
                mime_type: admitted.mime_type,
                size_bytes: candidates[index].size_bytes,
                local_path: readable[index].path.clone(),
                status: "failed".into(),
                error: Some("Connect to upload".into()),
            });
            added.push(id);
        }
        self.state.drafts.insert(key.clone(), draft);
        if self.state.connected {
            for id in added {
                self.begin_attachment(key.clone(), id)?;
            }
        }
        match admission.error.or(error) {
            Some(error) => Err(invalid(error)),
            None => Ok(()),
        }
    }

    pub(super) fn begin_attachment(&mut self, key: String, id: String) -> Result<(), PeerError> {
        let sender = self.sender.clone();
        let epoch = self.epoch;
        let attachment = self
            .state
            .drafts
            .get_mut(&key)
            .and_then(|draft| draft.attachments.iter_mut().find(|a| a.id == id))
            .ok_or_else(|| invalid("Attachment is unavailable"))?;
        let (source, name, mime) = (
            attachment.local_path.clone(),
            attachment.name.clone(),
            attachment.mime_type.clone(),
        );
        let network = self
            .network
            .as_mut()
            .filter(|_| self.state.connected)
            .ok_or_else(|| invalid("Connect to the Host to upload attachments"))?;
        let (peer, session) = (network.peer.clone(), network.session.clone());
        let (task_key, task_id) = (key.clone(), id.clone());
        network.spawn(async move {
            let (key, id) = (task_key, task_id);
            let result = agent_transport::transfers::upload_attachment(
                &peer,
                || async { session.open_stream().await.map_err(std::io::Error::other) },
                std::path::Path::new(&source),
                &name,
                &mime,
            )
            .await
            .map_err(invalid)
            .and_then(|uploaded| {
                let remote = uploaded
                    .attachment
                    .ok_or_else(|| invalid("Host did not return an attachment"))?;
                Ok(agent_domain::Attachment {
                    path: String::new(),
                    ..remote
                })
            });
            let _ = sender
                .send(Event::AttachmentFinished(epoch, key, id, result))
                .await;
        });
        let attachment = self
            .state
            .drafts
            .get_mut(&key)
            .and_then(|draft| draft.attachments.iter_mut().find(|a| a.id == id))
            .expect("attachment checked above");
        attachment.status = "uploading".into();
        attachment.error = None;
        Ok(())
    }

    pub(super) fn attachment_finished(
        &mut self,
        key: String,
        id: String,
        result: Result<agent_domain::Attachment, PeerError>,
    ) {
        let Some(attachment) = self
            .state
            .drafts
            .get_mut(&key)
            .and_then(|draft| draft.attachments.iter_mut().find(|a| a.id == id))
        else {
            return;
        };
        match result {
            Ok(remote) => {
                attachment.remote_id = Some(remote.id);
                attachment.kind = match remote.kind {
                    agent_domain::AttachmentKind::Image => "image",
                    agent_domain::AttachmentKind::File => "file",
                }
                .into();
                attachment.mime_type = remote.mime_type;
                attachment.size_bytes = remote.size;
                attachment.status = "ready".into();
                attachment.error = None;
            }
            Err(error) => {
                attachment.status = "failed".into();
                attachment.error = Some(crate::presentation::error::error_message(
                    &error.to_string(),
                ));
            }
        }
    }

    fn peripheral(&mut self, intent: Intent) -> Result<Next, PeerError> {
        Ok(match intent {
            Intent::Transcribe {
                mut draft_key,
                preparation,
                audio,
            } => {
                if self.state.selected_thread.is_none()
                    && self
                        .state
                        .drafts
                        .get(&draft_key)
                        .is_none_or(|draft| draft.project_id.is_none())
                {
                    self.ensure_new_thread_draft();
                    draft_key = self.state.draft_key();
                }
                let draft = self
                    .state
                    .drafts
                    .get(&draft_key)
                    .cloned()
                    .unwrap_or_else(|| self.state.current_draft());
                Next::call(
                    Call::Transcribe(op::Transcribe { preparation, audio }),
                    Some((draft_key, draft)),
                )
            }
            Intent::SearchContents {
                cwd,
                query,
                limit,
                case_sensitive,
                whole_word,
                use_regex,
            } => {
                let request = agent_protocol::workspace::SearchContents {
                    cwd: cwd.clone(),
                    query: query.clone(),
                    limit,
                    case_sensitive,
                    whole_word,
                    use_regex,
                };
                request.validate().map_err(invalid)?;
                self.state.sources.content_search.wanted = Some(
                    crate::state::ContentSearchQuery {
                        cwd,
                        query,
                        limit,
                        case_sensitive,
                        whole_word,
                        use_regex,
                    },
                );
                self.state.sources.content_search.in_flight = true;
                self.state.sources.content_search.error = None;
                Next::call(Call::SearchContents(request), None)
            }
            Intent::ListFiles { path } => {
                self.state.workspace.requested_directory = Some(path.clone());
                Next::call(Call::ListFiles(op::ListFiles { path }), None)
            }
            Intent::ReadFile {
                path,
                discard_draft,
            } => {
                self.state.workspace.requested_file = Some(path.clone());
                if discard_draft {
                    self.state.workspace.file_drafts.remove(&path);
                }
                Next::call(Call::ReadFile(op::ListFiles { path }), None)
            }
            Intent::EditFile { path, text } => {
                let file = self
                    .state
                    .workspace
                    .file
                    .as_ref()
                    .filter(|file| file.path == path)
                    .ok_or_else(|| invalid("Open the file before editing"))?;
                let revision = file.revision.clone();
                self.state
                    .workspace
                    .file_drafts
                    .entry(path)
                    .and_modify(|draft| Arc::make_mut(draft).text = text.clone())
                    .or_insert_with(|| Arc::new(FileDraft { text, revision }));
                Next::Done
            }
            Intent::SaveFile { path } => {
                let draft = self
                    .state
                    .workspace
                    .file_drafts
                    .get(&path)
                    .cloned()
                    .ok_or_else(|| invalid("File has no edits"))?;
                Next::call(
                    Call::WriteFile(op::WriteFile {
                        path,
                        revision: draft.revision.clone(),
                        text: draft.text.clone(),
                    }),
                    None,
                )
            }
            Intent::ReviewWorkspace { cwd } => {
                self.state.workspace.diff_retry_when_cwd_available = false;
                self.state.workspace.diff_request = None;
                self.state.workspace.review = None;
                Next::call(Call::ReviewWorkspace(op::ReviewWorkspace { cwd }), None)
            }
            Intent::SubscribeVcsStatus { cwd } => {
                self.subscribe_vcs_status(cwd);
                Next::Done
            }
            Intent::RefreshVcsStatus { cwd } => self.prepare_vcs_intent(
                Intent::RefreshVcsStatus { cwd },
            )?,
            intent @ Intent::LoadVcsRefs { .. }
            | intent @ Intent::SwitchVcsRef { .. }
            | intent @ Intent::CreateVcsRef { .. } => self.prepare_vcs_intent(intent)?,
            Intent::PullVcs { cwd } => self.prepare_vcs_intent(Intent::PullVcs { cwd })?,
            intent @ Intent::RunVcsAction { .. } => self.prepare_vcs_intent(intent)?,
            intent @ Intent::InitRepository { .. }
            | intent @ Intent::CreateVcsWorktree { .. }
            | intent @ Intent::RemoveVcsWorktree { .. }
            | intent @ Intent::ResolvePullRequest { .. }
            | intent @ Intent::PreparePullRequestThread { .. }
            | intent @ Intent::PublishRepository { .. } => self.prepare_vcs_intent(intent)?,
            Intent::ReadTurnDiff {
                from_run_ordinal,
                to_run_ordinal,
                ignore_whitespace,
            } => {
                self.state.workspace.diff_retry_when_cwd_available = false;
                let request = c::GetTurnDiff {
                    thread_id: self.selected()?,
                    from_run_ordinal,
                    to_run_ordinal,
                    ignore_whitespace: Some(ignore_whitespace),
                };
                self.state.workspace.review = None;
                self.state.workspace.diff_request = Some(request.clone());
                Next::call(Call::GetTurnDiff(request), None)
            }
            Intent::LoadWorktreeSettings => {
                Next::call(Call::ReadWorktreeSettings(m::Empty {}), None)
            }
            Intent::SaveWorktreeSettings { settings } => {
                Next::call(Call::UpdateWorktreeSettings(settings), None)
            }
            Intent::ListWorktrees => Next::call(Call::ListWorktrees(m::Empty {}), None),
            Intent::RemoveWorktree { path } => {
                Next::call(Call::RemoveWorktree(op::RemoveWorktree { path }), None)
            }
            Intent::SaveScheduledTask { draft } => {
                let request = crate::view::scheduled_tasks::upsert(&draft, self.new_command_id())
                    .map_err(invalid)?;
                Next::call(Call::UpsertScheduledTask(request), None)
            }
            Intent::SetScheduledTaskEnabled { id, enabled } => Next::call(
                Call::SetScheduledTaskEnabled(
                    agent_protocol::scheduled_tasks::SetScheduledTaskEnabled { id, enabled },
                ),
                None,
            ),
            Intent::DeleteScheduledTask { id } => Next::call(
                Call::DeleteScheduledTask(agent_protocol::scheduled_tasks::ScheduledTaskRef { id }),
                None,
            ),
            Intent::RunScheduledTaskNow { id } => Next::call(
                Call::RunScheduledTaskNow(
                    agent_protocol::scheduled_tasks::ScheduledTaskRef { id },
                ),
                None,
            ),
            Intent::LoadAccounts => Next::call(Call::ListAccounts(m::Empty {}), None),
            Intent::LoadUsageSummary { input } => {
                self.state.usage_loading = true;
                self.state.usage_error = None;
                Next::call(
                    Call::ReadUsageSummary(op::ReadUsageSummary {
                        input: input.into(),
                    }),
                    None,
                )
            }
            Intent::SetUsagePreferences { preferences } => {
                self.state.preferences.usage = preferences;
                Next::Done
            }
            Intent::RefreshUsageRates => {
                self.state.usage_loading = true;
                self.state.usage_error = None;
                Next::call(Call::RefreshUsageRates(op::RefreshUsageRates {}), None)
            }
            Intent::ConsumeResetCredit {
                provider,
                account_id,
                credit_id,
            } => Next::call(
                Call::ConsumeResetCredit(op::ConsumeResetCredit {
                    provider,
                    account_id,
                    credit_id,
                }),
                None,
            ),
            Intent::LoadProviders => Next::call(Call::ListProviders(m::Empty {}), None),
            Intent::SelectAccount { provider, id } => Next::call(
                Call::SelectAccount(op::SelectAccount { provider, id }),
                None,
            ),
            Intent::StartLogin { provider } => Next::call(
                Call::StartAccountLogin(op::StartAccountLogin { provider }),
                None,
            ),
            Intent::CompleteLogin { provider, id, code } => Next::call(
                Call::SubmitAccountLogin(op::SubmitAccountLogin { provider, id, code }),
                None,
            ),
            Intent::CancelLogin { provider, id } => Next::call(
                Call::CancelAccountLogin(op::CancelAccountLogin { provider, id }),
                None,
            ),
            Intent::DeleteAccount { provider, id } => Next::call(
                Call::LogoutAccount(op::LogoutAccount { provider, id }),
                None,
            ),
            Intent::LoadHostStatus => Next::call(Call::HostStatus(m::Empty {}), None),
            Intent::LoadUpdateStatus { target } => Next::call(
                Call::ReadUpdateStatus(m::UpdateStatusRequest { target }),
                None,
            ),
            Intent::CheckUpdate { request } => Next::call(Call::CheckUpdate(request), None),
            Intent::DownloadUpdate { target } => Next::call(
                Call::DownloadUpdate(m::UpdateActionRequest { target }),
                None,
            ),
            Intent::InstallUpdate { target } => {
                Next::call(Call::InstallUpdate(m::UpdateActionRequest { target }), None)
            }
            Intent::SetUpdateChannel { target, channel } => Next::call(
                Call::SetUpdateChannel(m::UpdateChannelRequest { target, channel }),
                None,
            ),
            Intent::LoadNativeUpdate { request } => {
                Next::call(Call::ReadNativeUpdate(request), None)
            }
            Intent::LoadRemoteHosts => Next::call(Call::ListRemotes(m::Empty {}), None),
            Intent::LoadHostManagement => {
                self.job(Call::HostStatus(m::Empty {}), None, None);
                Next::call(Call::ListRemotes(m::Empty {}), None)
            }
            Intent::RemoveRemoteHost { id } => {
                Next::call(Call::RemoveRemote(op::RemoveRemoteHost { id }), None)
            }
            Intent::CreateInvitation => Next::call(Call::Invite(m::Empty {}), None),
            Intent::RevokeDevice { id } => Next::call(Call::Revoke(op::RevokeDevice { id }), None),
            Intent::LoadDevices => Next::call(Call::DeviceList(d::DeviceListInput::default()), None),
            Intent::InspectDevices { host_id: _ } => Next::call(Call::DeviceList(d::DeviceListInput { inspect_only: true, ..Default::default() }), None),
            Intent::UpdateDeviceTool { host_id: _, tool } => Next::call(Call::DeviceList(d::DeviceListInput { update_tool: Some(match tool.as_str() { "hub" => d::DeviceTool::Hub, "agent" => d::DeviceTool::Agent, _ => return Err(invalid("Unknown device tool")), }), ..Default::default() }), None),
            Intent::RetryDeviceHost { host_id } => Next::call(Call::DeviceList(d::DeviceListInput { retry_host_id: Some(host_id), ..Default::default() }), None),
            Intent::ConfigureDevices {
                enabled,
                agent_access_enabled,
                onboarding_completed,
            } => Next::call(
                Call::DeviceConfigure(d::DeviceConfigureInput {
                    enabled,
                    agent_access_enabled,
                    onboarding_completed,
                }),
                None,
            ),
            Intent::UpdateDeviceHosts { hosts } => Next::call(
                Call::DeviceHosts(d::DeviceHostsInput {
                    hosts: hosts
                        .into_iter()
                        .map(|host| d::DeviceHostConfig {
                            id: host.id,
                            label: host.label,
                            target: host.target,
                            identity_file: host.identity_file,
                            port: host.port,
                        })
                        .collect(),
                }),
                None,
            ),
            Intent::OpenDevice {
                host_id,
                device_id,
                platform,
                boot,
            } => Next::call(
                Call::DeviceOpen(d::DeviceOpenInput {
                    thread_id: self.selected()?,
                    host_id,
                    device_id,
                    platform: device_platform(&platform)?,
                    boot,
                }),
                None,
            ),
            Intent::CloseDevice {
                host_id,
                device_id,
                shutdown,
            } => Next::call(
                Call::DeviceClose(d::DeviceCloseInput {
                    thread_id: self.selected()?,
                    host_id,
                    device_id,
                    shutdown,
                }),
                None,
            ),
            Intent::LoadDeviceDetail { host_id, device_id } => Next::call(
                Call::DeviceDetail(d::DeviceDetailInput { host_id, device_id }),
                None,
            ),
            Intent::DeviceAction {
                host_id,
                device_id,
                action,
            } => {
                let action = device_action(action)?;
                match action {
                    d::DeviceActionKind::Input(input) => Next::call(
                        Call::DeviceInput(d::DeviceInput { host_id, device_id, input }),
                        None,
                    ),
                    action => Next::call(
                        Call::DeviceAction(d::DeviceActionInput { host_id, device_id, action }),
                        None,
                    ),
                }
            }
            Intent::CaptureDeviceScreenshot { host_id, device_id } => Next::call(
                Call::DeviceScreenshot(d::DeviceScreenshotInput { host_id, device_id }),
                None,
            ),
            Intent::LoadDeviceAccessibility { host_id, device_id } => Next::call(
                Call::DeviceAccessibility(d::DeviceAccessibilityInput { host_id, device_id }),
                None,
            ),
            Intent::LoadDeviceEventLog { host_id, device_id, limit } => Next::call(
                Call::DeviceEventLog(d::DeviceEventLogInput { host_id, device_id, limit }),
                None,
            ),
            Intent::StartDeviceRecording { host_id, device_id, format } => Next::call(
                Call::DeviceRecordingStart(d::DeviceRecordingStartInput { thread_id: self.selected()?, host_id, device_id, format: device_recording_format(&format)? }),
                None,
            ),
            Intent::StopDeviceRecording { host_id, device_id, recording_id, session_epoch } => Next::call(
                Call::DeviceRecordingStop(d::DeviceRecordingStopInput { thread_id: self.selected()?, host_id, device_id, recording_id, session_epoch }),
                None,
            ),
            Intent::SubscribeDevice => {
                let thread = self.selected()?;
                self.subscribe_device(&thread);
                Next::Done
            }
            Intent::UnsubscribeDevice => {
                if let Some(thread) = &self.state.selected_thread {
                    self.close_stream(&super::owner::StreamKey::Device(thread.clone()));
                }
                Next::Done
            }
            Intent::AddProject { path } => {
                Next::call(Call::AddProject(op::AddProject { cwd: path }), None)
            }
            Intent::PreviewList { configured_urls } => {
                self.state.preview.configured_urls = configured_urls.clone();
                if let Some(thread) = self.state.selected_thread.clone() {
                    self.close_stream(&super::owner::StreamKey::Preview(thread.clone()));
                    self.subscribe_preview(&thread);
                }
                Next::call(
                    Call::PreviewList(agent_protocol::preview::PreviewList {
                        thread_id: self.selected()?,
                        configured_urls,
                    }),
                    None,
                )
            }
            Intent::PreviewOpen {
                url,
                viewport,
                appearance,
                zoom,
            } => {
                let request = agent_protocol::preview::PreviewOpen {
                    thread_id: self.selected()?,
                    url,
                    viewport,
                    appearance,
                    zoom,
                    rendered_size: None,
                };
                request.validate().map_err(invalid)?;
                Next::call(Call::PreviewOpen(request), None)
            }
            Intent::PreviewNavigate { tab_id, url } => {
                let request = agent_protocol::preview::PreviewNavigate {
                    thread_id: self.selected()?,
                    tab_id,
                    url,
                };
                request.validate().map_err(invalid)?;
                Next::call(Call::PreviewNavigate(request), None)
            }
            Intent::PreviewResize {
                tab_id,
                viewport,
                rendered_width,
                rendered_height,
            } => {
                let rendered_size = match (rendered_width, rendered_height) {
                    (Some(width), Some(height)) => Some(
                        agent_protocol::preview::PreviewRenderedViewportSize { width, height },
                    ),
                    (None, None) => None,
                    _ => return Err(invalid("measured preview viewport dimensions must be paired")),
                };
                let request = agent_protocol::preview::PreviewResize {
                    thread_id: self.selected()?,
                    tab_id,
                    viewport,
                    rendered_size,
                };
                request.validate().map_err(invalid)?;
                Next::call(Call::PreviewResize(request), None)
            }
            Intent::PreviewSetAppearance { tab_id, appearance } => {
                let request = agent_protocol::preview::PreviewSetAppearance {
                    thread_id: self.selected()?,
                    tab_id,
                    appearance,
                };
                request.validate().map_err(invalid)?;
                Next::call(Call::PreviewSetAppearance(request), None)
            }
            Intent::PreviewSetZoom { tab_id, zoom } => {
                let request = agent_protocol::preview::PreviewSetZoom {
                    thread_id: self.selected()?,
                    tab_id,
                    zoom,
                };
                request.validate().map_err(invalid)?;
                Next::call(Call::PreviewSetZoom(request), None)
            }
            Intent::PreviewRefresh { tab_id } => {
                let request = agent_protocol::preview::PreviewTab {
                    thread_id: self.selected()?,
                    tab_id,
                };
                request.validate().map_err(invalid)?;
                Next::call(Call::PreviewRefresh(request), None)
            }
            Intent::PreviewClose { tab_id } => {
                let request = agent_protocol::preview::PreviewClose {
                    thread_id: self.selected()?,
                    tab_id,
                };
                request.validate().map_err(invalid)?;
                Next::call(Call::PreviewClose(request), None)
            }
            Intent::PreviewRecordingStart { tab_id } => {
                let request = agent_protocol::preview::PreviewRecordingStart {
                    thread_id: self.selected()?,
                    tab_id,
                };
                request.validate().map_err(invalid)?;
                Next::call(Call::PreviewRecordingStart(request), None)
            }
            Intent::PreviewRecordingStop { tab_id } => {
                let request = agent_protocol::preview::PreviewRecordingStop {
                    thread_id: self.selected()?,
                    tab_id,
                };
                request.validate().map_err(invalid)?;
                Next::call(Call::PreviewRecordingStop(request), None)
            }
            _ => unreachable!("conversation intents are prepared above"),
        })
    }
}

fn parse_pull_request_action(value: &str) -> Result<agent_domain::PullRequestAction, PeerError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "merge" => Ok(agent_domain::PullRequestAction::Merge),
        "mark_ready" | "ready" => Ok(agent_domain::PullRequestAction::MarkReady),
        "mark_draft" | "draft" => Ok(agent_domain::PullRequestAction::MarkDraft),
        "close" => Ok(agent_domain::PullRequestAction::Close),
        "reopen" => Ok(agent_domain::PullRequestAction::Reopen),
        "update_branch" => Ok(agent_domain::PullRequestAction::UpdateBranch),
        "enable_auto_merge" => Ok(agent_domain::PullRequestAction::EnableAutoMerge),
        "disable_auto_merge" => Ok(agent_domain::PullRequestAction::DisableAutoMerge),
        "revert" => Ok(agent_domain::PullRequestAction::Revert),
        _ => Err(invalid("unknown pull request action")),
    }
}

fn parse_pull_request_merge_method(
    value: &str,
) -> Result<pr::PullRequestMergeMethod, PeerError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "merge" => Ok(pr::PullRequestMergeMethod::Merge),
        "squash" => Ok(pr::PullRequestMergeMethod::Squash),
        "rebase" => Ok(pr::PullRequestMergeMethod::Rebase),
        _ => Err(invalid("unknown pull request merge method")),
    }
}

fn parse_pull_request_verdict(value: &str) -> Result<pr::PullRequestReviewVerdict, PeerError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "approve" => Ok(pr::PullRequestReviewVerdict::Approve),
        "request_changes" | "changes" => Ok(pr::PullRequestReviewVerdict::RequestChanges),
        "comment" => Ok(pr::PullRequestReviewVerdict::Comment),
        _ => Err(invalid("unknown pull request review verdict")),
    }
}
