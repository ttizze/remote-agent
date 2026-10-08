//! Device state the views read beside the Host's: preferences, holds over the
//! list, answer drafts, panel selections and flows in progress.
use crate::view::projects::import::{ImportToast, SessionImportProgress};
use crate::view::{
    models::ordering::FavoriteModel, requests::QuestionDraft, thread_order::PendingThreadOrder,
    time::TimestampFormat,
};
use agent_domain::{CommandId, ThreadId};
use agent_protocol::conversation::SessionScan;
use agent_protocol::device::{DeviceAccessibilityTree, DeviceDetail, DeviceEvent, DeviceEventLogEntry, DeviceForegroundUpdate, DeviceFrame, DeviceRecording, DeviceScreenshot, DeviceScreenConfig, DeviceServiceState, DeviceSession, DeviceVideoFrame};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

const MAX_VIDEO_EVENTS_PER_STREAM: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DeviceProjectedPoint {
    pub x: f32,
    pub y: f32,
}

/// Project a screen-space pointer into a device frame while preserving the
/// letterbox. Points in the surrounding letterbox are rejected so callers can
/// end a gesture at the last valid device coordinate instead of clamping an
/// outside click to an edge.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn project_device_point(
    view_width: f32,
    view_height: f32,
    frame_width: f32,
    frame_height: f32,
    point_x: f32,
    point_y: f32,
) -> Option<DeviceProjectedPoint> {
    if ![view_width, view_height, frame_width, frame_height, point_x, point_y]
        .iter()
        .all(|value| value.is_finite() && *value > 0.0)
    {
        return None;
    }
    let scale = (view_width / frame_width).min(view_height / frame_height);
    let rendered_width = frame_width * scale;
    let rendered_height = frame_height * scale;
    let offset_x = (view_width - rendered_width) / 2.0;
    let offset_y = (view_height - rendered_height) / 2.0;
    let x = (point_x - offset_x) / rendered_width;
    let y = (point_y - offset_y) / rendered_height;
    if !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
        return None;
    }
    Some(DeviceProjectedPoint { x, y })
}

/// The core owns the single-flight Duo control queue. Native surfaces can
/// issue slider/preset changes without racing the Host acknowledgement: one
/// request is active and the newest queued value replaces older values.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceDuoControlState {
    pub pending: bool,
    pub requested: Option<crate::state::DeviceDuoCommandIntent>,
    pub error: Option<String>,
    queued: Option<crate::state::DeviceDuoCommandIntent>,
    active_request_id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceDuoRequest {
    pub thread_id: ThreadId,
    pub host_id: Option<String>,
    pub device_id: String,
    pub session_epoch: String,
    pub request_id: u64,
    pub command: crate::state::DeviceDuoCommandIntent,
}

/// Settings this device keeps across launches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preferences {
    pub timestamp_format: TimestampFormat,
    pub favorite_models: Vec<FavoriteModel>,
    /// The user's model order per provider instance, by slug.
    pub model_order: BTreeMap<String, Vec<String>>,
    /// The Working section (beta).
    pub working_section: bool,
    pub diff_ignore_whitespace: bool,
    /// The script each project last ran, by project id.
    pub last_run_scripts: BTreeMap<String, String>,
    /// Provider instances whose resume dialog was told never to ask again.
    pub resume_compaction_dismissed: BTreeSet<String>,
    /// The terminal's text size in points; `None` keeps the default.
    pub terminal_font_size: Option<f64>,
    /// Each model's last chosen options, which a newly picked model takes.
    pub model_options: crate::view::models::staging::ModelOptionMemory,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            timestamp_format: TimestampFormat::default(),
            favorite_models: vec![],
            model_order: BTreeMap::new(),
            working_section: false,
            diff_ignore_whitespace: true,
            last_run_scripts: BTreeMap::new(),
            resume_compaction_dismissed: BTreeSet::new(),
            terminal_font_size: None,
            model_options: BTreeMap::new(),
        }
    }
}

/// A list reorder held on screen until its key writes land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadOrderHold {
    pub order: PendingThreadOrder,
    pub commands: Vec<CommandId>,
}

/// One question request's answers being written, and the question shown.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuestionDrafts {
    pub drafts: Vec<QuestionDraft>,
    pub question_index: u32,
}
impl QuestionDrafts {
    pub fn get(&self, question_id: &str) -> Option<&QuestionDraft> {
        self.drafts
            .iter()
            .find(|draft| draft.question_id == question_id)
    }
    pub fn set(&mut self, draft: QuestionDraft) {
        match self
            .drafts
            .iter_mut()
            .find(|existing| existing.question_id == draft.question_id)
        {
            Some(existing) => *existing = draft,
            None => self.drafts.push(draft),
        }
    }
}

/// The draft key holding the files attached to one question's answer.
pub fn answer_draft_key(request_id: &str, question_id: &str) -> String {
    format!("answer:{request_id}:{question_id}")
}

/// The agent-session import step of adding projects.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionImport {
    pub scan: Option<SessionScan>,
    pub scan_pending: bool,
    pub scan_error: Option<String>,
    /// `None` until the user changes the default selection.
    pub selection: Option<BTreeSet<String>>,
    pub importing: bool,
    pub progress: SessionImportProgress,
    /// Shown once the landing project opened.
    pub toast: Option<ImportToast>,
}

/// Host-owned device state folded into the client snapshot. Device commands
/// remain typed protocol calls; this record only retains the latest state and
/// frames for native views.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeviceState {
    pub service: Option<DeviceServiceState>,
    /// Monotonic identity for accepted frame/video events. Service revisions
    /// describe configuration and discovery, so native frame consumers must
    /// observe this owner-controlled value instead of polling that revision.
    pub frame_revision: u64,
    pub sessions: Vec<DeviceSession>,
    pub details: BTreeMap<(String, String), DeviceDetail>,
    pub frames: BTreeMap<(String, String, String), DeviceFrame>,
    pub video_frames: BTreeMap<(String, String, String, u8), DeviceVideoFrame>,
    /// Ordered access units retained for stateful native decoders. The latest
    /// frame map above remains the cheap projection used by still-image views.
    pub video_events: BTreeMap<(String, String, String, u8), VecDeque<DeviceVideoFrame>>,
    pub accessibility: BTreeMap<(String, String), DeviceAccessibilityTree>,
    pub event_log: BTreeMap<(String, String), Vec<DeviceEventLogEntry>>,
    pub foreground: BTreeMap<(String, String), DeviceForegroundUpdate>,
    pub screens: BTreeMap<(String, String, String, u8), DeviceScreenConfig>,
    pub recordings: BTreeMap<(String, String, String), agent_protocol::device::DeviceRecordingStatus>,
    pub duo_controls: BTreeMap<(String, String, String, String), DeviceDuoControlState>,
    duo_request_sequence: u64,
    pub last_recording: Option<DeviceRecording>,
    pub last_screenshot: Option<DeviceScreenshot>,
    pub error: Option<String>,
}

impl DeviceState {
    pub fn enqueue_duo(
        &mut self,
        thread_id: ThreadId,
        host_id: Option<String>,
        device_id: String,
        command: crate::state::DeviceDuoCommandIntent,
    ) -> Result<Option<DeviceDuoRequest>, String> {
        let effective_host = host_id
            .as_deref()
            .unwrap_or(agent_protocol::device::LOCAL_DEVICE_HOST_ID);
        let session = self
            .sessions
            .iter()
            .find(|session| {
                session.thread_id == thread_id
                    && session.host_id == effective_host
                    && session.device_id == device_id
            })
            .cloned()
            .ok_or_else(|| "device session is not open".to_owned())?;
        let key = (
            thread_id.to_string(),
            effective_host.to_owned(),
            device_id.clone(),
            session.opened_at.clone(),
        );
        if self.duo_controls.get(&key).is_some_and(|control| control.pending) {
            self.duo_controls
                .get_mut(&key)
                .expect("Duo control state exists")
                .queued = Some(command);
            return Ok(None);
        }
        self.duo_request_sequence = self.duo_request_sequence.saturating_add(1).max(1);
        let request_id = self.duo_request_sequence;
        let control = self.duo_controls.entry(key).or_insert_with(|| DeviceDuoControlState {
            pending: false,
            requested: None,
            error: None,
            queued: None,
            active_request_id: None,
        });
        control.error = None;
        control.pending = true;
        control.requested = Some(command.clone());
        control.active_request_id = Some(request_id);
        Ok(Some(DeviceDuoRequest {
            thread_id,
            host_id,
            device_id,
            session_epoch: session.opened_at,
            request_id,
            command,
        }))
    }

    pub fn complete_duo(
        &mut self,
        request: &DeviceDuoRequest,
        accepted: bool,
        error: Option<String>,
    ) -> Option<DeviceDuoRequest> {
        let key = (
            request.thread_id.to_string(),
            request
                .host_id
                .as_deref()
                .unwrap_or(agent_protocol::device::LOCAL_DEVICE_HOST_ID)
                .to_owned(),
            request.device_id.clone(),
            request.session_epoch.clone(),
        );
        let queued = {
            let control = self.duo_controls.get_mut(&key)?;
            if control.active_request_id != Some(request.request_id) {
                return None;
            }
            if !accepted {
                control.pending = false;
                control.requested = None;
                control.queued = None;
                control.active_request_id = None;
                control.error = Some(error.unwrap_or_else(|| "device Duo control failed".into()));
                return None;
            }
            control.queued.take()
        };
        if let Some(command) = queued {
            self.duo_request_sequence = self.duo_request_sequence.saturating_add(1).max(1);
            let request_id = self.duo_request_sequence;
            let control = self.duo_controls.get_mut(&key).expect("Duo control state exists");
            control.requested = Some(command.clone());
            control.active_request_id = Some(request_id);
            return Some(DeviceDuoRequest {
                thread_id: request.thread_id.clone(),
                host_id: request.host_id.clone(),
                device_id: request.device_id.clone(),
                session_epoch: request.session_epoch.clone(),
                request_id,
                command,
            });
        }
        let control = self.duo_controls.get_mut(&key).expect("Duo control state exists");
        control.pending = false;
        control.requested = None;
        control.active_request_id = None;
        control.error = None;
        self.duo_controls.remove(&key);
        None
    }

    pub fn fail_duo(&mut self, request: &DeviceDuoRequest, error: impl Into<String>) {
        let _ = self.complete_duo(request, false, Some(error.into()));
    }

    pub fn complete_duo_input(
        &mut self,
        input: &agent_protocol::device::DeviceInput,
        result: &agent_protocol::device::DeviceControlResult,
    ) -> Option<DeviceDuoRequest> {
        let request_id = result.request_id.or(input.request_id)?;
        let thread_id = input.thread_id.as_ref()?;
        let effective_host = input
            .host_id
            .as_deref()
            .unwrap_or(agent_protocol::device::LOCAL_DEVICE_HOST_ID);
        let (key, command) = self
            .duo_controls
            .iter()
            .find(|((thread, host, device, _), state)| {
                thread == &thread_id.to_string()
                    && host == effective_host
                    && device == &input.device_id
                    && state.active_request_id == Some(request_id)
            })
            .map(|(key, state)| (key.clone(), state.requested.clone()))?;
        let request = DeviceDuoRequest {
            thread_id: thread_id.clone(),
            host_id: input.host_id.clone(),
            device_id: input.device_id.clone(),
            session_epoch: key.3.clone(),
            request_id,
            command: command?,
        };
        self.complete_duo(&request, result.accepted, result.error.clone())
    }

    pub fn fail_duo_input(&mut self, input: &agent_protocol::device::DeviceInput, error: impl Into<String>) {
        let Some(request_id) = input.request_id else { return };
        let Some(thread_id) = input.thread_id.as_ref() else { return };
        let effective_host = input
            .host_id
            .as_deref()
            .unwrap_or(agent_protocol::device::LOCAL_DEVICE_HOST_ID);
        let Some((key, command)) = self
            .duo_controls
            .iter()
            .find(|((thread, host, device, _), state)| {
                thread == &thread_id.to_string()
                    && host == effective_host
                    && device == &input.device_id
                    && state.active_request_id == Some(request_id)
            })
            .map(|(key, state)| (key.clone(), state.requested.clone()))
        else {
            return;
        };
        let Some(command) = command else { return };
        self.fail_duo(
            &DeviceDuoRequest {
                thread_id: thread_id.clone(),
                host_id: input.host_id.clone(),
                device_id: input.device_id.clone(),
                session_epoch: key.3,
                request_id,
                command,
            },
            error,
        );
    }

    pub fn clear_duo_for_closed_sessions(&mut self) {
        let sessions = self.sessions.clone();
        self.duo_controls.retain(|(thread, host, device, epoch), _| {
            sessions.iter().any(|session| {
                session.thread_id.to_string() == *thread
                    && session.host_id == *host
                    && session.device_id == *device
                    && session.opened_at == *epoch
            })
        });
    }

    fn has_session(&self, thread_id: &agent_domain::ThreadId, host_id: &str, device_id: &str, epoch: &str) -> bool {
        self.sessions.iter().any(|session| {
            &session.thread_id == thread_id
                && session.host_id == host_id
                && session.device_id == device_id
                && session.opened_at == epoch
        })
    }

    fn has_device_session(&self, host_id: &str, device_id: &str, epoch: &str) -> bool {
        self.sessions.iter().any(|session| {
            session.host_id == host_id && session.device_id == device_id && session.opened_at == epoch
        })
    }

    pub fn apply_event(&mut self, event: DeviceEvent) {
        match event {
            DeviceEvent::State(service) => {
                self.sessions = service.sessions.clone();
                self.service = Some(service);
                let mut frame_stream_changed = false;
                let active = self
                    .sessions
                    .iter()
                    .map(|session| {
                        (
                            session.thread_id.to_string(),
                            session.host_id.clone(),
                            session.device_id.clone(),
                            session.opened_at.clone(),
                        )
                    })
                    .collect::<std::collections::BTreeSet<_>>();
                self.frames.retain(|key, frame| {
                    let keep = active.contains(&(key.0.clone(), key.1.clone(), key.2.clone(), frame.session_epoch.clone()));
                    frame_stream_changed |= !keep;
                    keep
                });
                self.video_frames.retain(|key, frame| {
                    let keep = active.contains(&(key.0.clone(), key.1.clone(), key.2.clone(), frame.session_epoch.clone()));
                    frame_stream_changed |= !keep;
                    keep
                });
                self.video_events.retain(|key, events| {
                    events.retain(|frame| {
                        let keep = active.contains(&(key.0.clone(), key.1.clone(), key.2.clone(), frame.session_epoch.clone()));
                        frame_stream_changed |= !keep;
                        keep
                    });
                    let keep = !events.is_empty();
                    frame_stream_changed |= !keep;
                    keep
                });
                let active_devices = self
                    .sessions
                    .iter()
                    .map(|session| (session.host_id.clone(), session.device_id.clone()))
                    .collect::<std::collections::BTreeSet<_>>();
                let active_epochs = self
                    .sessions
                    .iter()
                    .map(|session| {
                        (
                            (session.host_id.clone(), session.device_id.clone()),
                            session.opened_at.clone(),
                        )
                    })
                    .collect::<std::collections::BTreeMap<_, _>>();
                self.details
                    .retain(|key, _| active_devices.contains(key));
                self.accessibility.retain(|key, tree| {
                    active_devices.contains(key) && active_epochs.get(key) == Some(&tree.session_epoch)
                });
                self.event_log.retain(|key, entries| {
                    active_devices.contains(key)
                        && entries.iter().any(|entry| active_epochs.get(key) == Some(&entry.session_epoch))
                });
                self.foreground.retain(|key, update| {
                    active_devices.contains(key) && active_epochs.get(key) == Some(&update.session_epoch)
                });
                self.screens.retain(|key, screen| {
                    screen.thread_id.as_ref().is_some_and(|thread| {
                        active.contains(&(
                            thread.to_string(),
                            key.1.clone(),
                            key.2.clone(),
                            screen.session_epoch.clone().unwrap_or_default(),
                        ))
                    })
                });
                self.recordings.retain(|(thread, host, device), status| {
                    active.contains(&(thread.clone(), host.clone(), device.clone(), status.session_epoch.clone()))
                });
                self.clear_duo_for_closed_sessions();
                if self.last_recording.as_ref().is_some_and(|recording| {
                    !active.contains(&(
                        recording.status.thread_id.to_string(),
                        recording.status.host_id.clone(),
                        recording.status.device_id.clone(),
                        recording.status.session_epoch.clone(),
                    ))
                }) {
                    self.last_recording = None;
                }
                if frame_stream_changed {
                    self.frame_revision = self.frame_revision.saturating_add(1);
                }
                self.error = None;
            }
            DeviceEvent::Frame(frame) => {
                if !self.has_session(&frame.thread_id, &frame.device.host_id, &frame.device.id, &frame.session_epoch) {
                    return;
                }
                self.frames.insert(
                    (
                        frame.thread_id.to_string(),
                        frame.device.host_id.clone(),
                        frame.device.id.clone(),
                    ),
                    frame,
                );
                self.frame_revision = self.frame_revision.saturating_add(1);
            }
            DeviceEvent::Video(frame) => {
                if !self.has_session(&frame.thread_id, &frame.device.host_id, &frame.device.id, &frame.session_epoch) {
                    return;
                }
                let key = (
                    frame.thread_id.to_string(),
                    frame.device.host_id.clone(),
                    frame.device.id.clone(),
                    frame.screen_id.unwrap_or(0),
                );
                if self
                    .video_frames
                    .get(&key)
                    .is_some_and(|latest| frame.sequence <= latest.sequence)
                {
                    return;
                }
                self.video_frames.insert(key.clone(), frame.clone());
                let events = self.video_events.entry(key).or_default();
                events.push_back(frame);
                while events.len() > MAX_VIDEO_EVENTS_PER_STREAM {
                    events.pop_front();
                }
                self.frame_revision = self.frame_revision.saturating_add(1);
            }
            DeviceEvent::Accessibility(tree) => {
                if !self.has_device_session(&tree.host_id, &tree.device_id, &tree.session_epoch) {
                    return;
                }
                self.accessibility
                    .insert((tree.host_id.clone(), tree.device_id.clone()), tree);
            }
            DeviceEvent::EventLog(entry) => {
                if !self.has_device_session(&entry.host_id, &entry.device_id, &entry.session_epoch) {
                    return;
                }
                let log = self
                    .event_log
                    .entry((entry.host_id.clone(), entry.device_id.clone()))
                    .or_default();
                if !log.iter().any(|existing| existing.id == entry.id) {
                    log.push(entry);
                    log.sort_by_key(|entry| entry.id);
                    if log.len() > 100 {
                        let keep_from = log.len() - 100;
                        log.drain(..keep_from);
                    }
                }
            }
            DeviceEvent::Foreground(update) => {
                if !self.has_device_session(&update.host_id, &update.device_id, &update.session_epoch) {
                    return;
                }
                self.foreground.insert((update.host_id.clone(), update.device_id.clone()), update);
            }
            DeviceEvent::Screen(screen) => {
                let (Some(thread), Some(host), Some(device), Some(epoch)) = (
                    &screen.thread_id,
                    &screen.host_id,
                    &screen.device_id,
                    &screen.session_epoch,
                ) else {
                    return;
                };
                if !self.has_session(thread, host, device, epoch) {
                    return;
                }
                self.screens.insert(
                    (
                        thread.to_string(),
                        host.clone(),
                        device.clone(),
                        screen.screen_id.unwrap_or(0),
                    ),
                    screen,
                );
            }
            DeviceEvent::Recording(status) => {
                let key = (status.thread_id.to_string(), status.host_id.clone(), status.device_id.clone());
                if !self.has_session(&status.thread_id, &status.host_id, &status.device_id, &status.session_epoch) {
                    return;
                }
                if status.active {
                    self.recordings.insert(key, status);
                } else {
                    self.recordings.remove(&key);
                }
            }
            DeviceEvent::RecordingComplete(recording) => {
                let key = (
                    recording.status.thread_id.to_string(),
                    recording.status.host_id.clone(),
                    recording.status.device_id.clone(),
                );
                if !self.has_session(
                    &recording.status.thread_id,
                    &recording.status.host_id,
                    &recording.status.device_id,
                    &recording.status.session_epoch,
                ) {
                    return;
                }
                self.recordings.remove(&key);
                self.last_recording = Some(recording);
            }
        }
    }

    pub fn service(&self) -> DeviceServiceState {
        self.service.clone().unwrap_or_default()
    }

    pub fn session(&self, thread: &str) -> Option<&DeviceSession> {
        self.sessions
            .iter()
            .rev()
            .find(|session| session.thread_id.as_str() == thread)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::device::{DeviceFrame, DeviceFrameEncoding, DevicePlatform, DeviceRecording, DeviceRecordingFormat, DeviceRecordingStatus, DeviceSummary, DeviceVideoFrame};

    fn session(thread: &str, host: &str, device: &str) -> DeviceSession {
        DeviceSession {
            thread_id: agent_domain::ThreadId::new(thread).unwrap(),
            host_id: host.into(),
            device_id: device.into(),
            platform: DevicePlatform::Android,
            opened_at: "0".into(),
        }
    }

    #[test]
    fn device_projection_rejects_letterbox_and_preserves_frame_coordinates() {
        let point = project_device_point(100.0, 100.0, 100.0, 50.0, 50.0, 50.0).unwrap();
        assert_eq!(point, DeviceProjectedPoint { x: 0.5, y: 0.5 });
        assert!(project_device_point(100.0, 100.0, 100.0, 50.0, 50.0, 10.0).is_none());
        assert!(project_device_point(100.0, 100.0, 100.0, 50.0, 150.0, 50.0).is_none());
    }

    #[test]
    fn state_updates_prune_frames_for_closed_sessions() {
        let current = session("thread", "host", "device");
        let mut state = DeviceState::default();
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![current.clone()],
            ..DeviceServiceState::default()
        }));
        state.apply_event(DeviceEvent::Frame(DeviceFrame {
            thread_id: current.thread_id.clone(),
            session_epoch: current.opened_at.clone(),
            device: DeviceSummary {
                host_id: current.host_id.clone(),
                id: current.device_id.clone(),
                platform: current.platform,
                name: "Pixel".into(),
                version: "Android".into(),
                booted: true,
                physical: false,
            },
            png: vec![1],
            width: 1,
            height: 1,
            sequence: 1,
        }));
        assert_eq!(state.frames.len(), 1);
        state.apply_event(DeviceEvent::State(DeviceServiceState::default()));
        assert!(state.frames.is_empty());
        state.details.insert(
            (current.host_id.clone(), current.device_id.clone()),
            DeviceDetail {
                host_id: current.host_id,
                device_id: current.device_id,
                settings: Default::default(),
                foreground_app: None,
                read_at: "0".into(),
            },
        );
        state.apply_event(DeviceEvent::State(DeviceServiceState::default()));
        assert!(state.details.is_empty());
    }

    #[test]
    fn recording_events_keep_active_state_and_release_completed_bytes() {
        let current = session("thread", "host", "device");
        let mut state = DeviceState::default();
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![current.clone()],
            ..DeviceServiceState::default()
        }));
        let status = DeviceRecordingStatus {
            thread_id: current.thread_id.clone(),
            host_id: current.host_id.clone(),
            device_id: current.device_id.clone(),
            session_epoch: current.opened_at.clone(),
            format: DeviceRecordingFormat::Avcc,
            active: true,
            started_at: "0".into(),
            frame_count: 1,
            byte_count: 2,
            error: None,
        };
        state.apply_event(DeviceEvent::Recording(status.clone()));
        assert_eq!(state.recordings.len(), 1);
        state.apply_event(DeviceEvent::RecordingComplete(DeviceRecording { status: DeviceRecordingStatus { active: false, ..status }, bytes: vec![1, 2] }));
        assert!(state.recordings.is_empty());
        assert_eq!(state.last_recording.as_ref().unwrap().bytes, vec![1, 2]);
        state.apply_event(DeviceEvent::State(DeviceServiceState::default()));
        assert!(state.recordings.is_empty());
        assert!(state.last_recording.is_none());
    }

    #[test]
    fn late_video_events_are_rejected_after_session_close() {
        let current = session("thread", "host", "device");
        let mut state = DeviceState::default();
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![current.clone()],
            ..DeviceServiceState::default()
        }));
        let frame = DeviceVideoFrame {
            thread_id: current.thread_id.clone(),
            session_epoch: current.opened_at.clone(),
            device: DeviceSummary {
                host_id: current.host_id.clone(),
                id: current.device_id.clone(),
                platform: current.platform,
                name: "Pixel".into(),
                version: "Android".into(),
                booted: true,
                physical: false,
            },
            payload: vec![0, 0, 1, 0x65],
            encoding: DeviceFrameEncoding::H264,
            width: 2,
            height: 2,
            sequence: 1,
            timestamp_us: Some(1),
            keyframe: true,
            screen_id: Some(1),
        };
        state.apply_event(DeviceEvent::Video(frame.clone()));
        assert_eq!(state.video_events.values().map(|events| events.len()).sum::<usize>(), 1);
        assert_eq!(state.frame_revision, 1);
        state.apply_event(DeviceEvent::Video(frame.clone()));
        assert_eq!(state.frame_revision, 1);
        state.apply_event(DeviceEvent::State(DeviceServiceState::default()));
        state.apply_event(DeviceEvent::Video(frame));
        assert!(state.video_events.is_empty());
        assert_eq!(state.frame_revision, 1);

        let reopened = session("thread", "host", "device");
        let reopened = DeviceSession { opened_at: "1".into(), ..reopened };
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![reopened.clone()],
            ..DeviceServiceState::default()
        }));
        let stale = DeviceVideoFrame {
            thread_id: reopened.thread_id.clone(),
            session_epoch: "0".into(),
            device: DeviceSummary {
                host_id: reopened.host_id.clone(),
                id: reopened.device_id.clone(),
                platform: reopened.platform,
                name: "Pixel".into(),
                version: "Android".into(),
                booted: true,
                physical: false,
            },
            payload: vec![0, 0, 1, 0x65],
            encoding: DeviceFrameEncoding::H264,
            width: 2,
            height: 2,
            sequence: 99,
            timestamp_us: Some(99),
            keyframe: true,
            screen_id: Some(1),
        };
        state.apply_event(DeviceEvent::Video(stale));
        assert!(state.video_events.is_empty());
        assert_eq!(state.frame_revision, 1);
    }

    #[test]
    fn duo_controls_are_single_flight_and_keep_only_the_latest_queued_value() {
        let current = session("thread", "host", "device");
        let mut state = DeviceState::default();
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![current.clone()],
            ..DeviceServiceState::default()
        }));
        let first = state
            .enqueue_duo(
                current.thread_id.clone(),
                Some(current.host_id.clone()),
                current.device_id.clone(),
                crate::state::DeviceDuoCommandIntent::Angle { value: 30.0 },
            )
            .unwrap()
            .unwrap();
        assert!(state
            .enqueue_duo(
                current.thread_id.clone(),
                Some(current.host_id.clone()),
                current.device_id.clone(),
                crate::state::DeviceDuoCommandIntent::Angle { value: 60.0 },
            )
            .unwrap()
            .is_none());
        assert!(state
            .enqueue_duo(
                current.thread_id.clone(),
                Some(current.host_id.clone()),
                current.device_id.clone(),
                crate::state::DeviceDuoCommandIntent::Angle { value: 90.0 },
            )
            .unwrap()
            .is_none());
        let next = state.complete_duo(&first, true, None).unwrap();
        assert_eq!(next.command, crate::state::DeviceDuoCommandIntent::Angle { value: 90.0 });
        assert!(state.complete_duo(&next, true, None).is_none());
        assert!(state.duo_controls.is_empty());
    }

    #[test]
    fn duo_failure_is_visible_until_the_session_reconnects() {
        let current = session("thread", "host", "device");
        let mut state = DeviceState::default();
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![current.clone()],
            ..DeviceServiceState::default()
        }));
        let request = state
            .enqueue_duo(
                current.thread_id,
                Some(current.host_id),
                current.device_id,
                crate::state::DeviceDuoCommandIntent::Table { value: true },
            )
            .unwrap()
            .unwrap();
        state.fail_duo(&request, "control timed out");
        let control = state.duo_controls.values().next().unwrap();
        assert!(!control.pending);
        assert_eq!(control.error.as_deref(), Some("control timed out"));
        state.apply_event(DeviceEvent::State(DeviceServiceState::default()));
        assert!(state.duo_controls.is_empty());
    }
}
