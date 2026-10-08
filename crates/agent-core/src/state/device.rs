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

const MAX_VIDEO_EVENTS_PER_STREAM: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DeviceProjectedPoint {
    pub x: f32,
    pub y: f32,
}

/// Project a point through the actual frame rectangle. Letterbox points are
/// rejected so a gesture can end at its last valid device coordinate.
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
    // Keep the fit arithmetic in f64. Valid finite f32 dimensions can have
    // ratios below f32's subnormal range; doing the division in f32 would
    // collapse the rendered rectangle to zero and produce NaN coordinates.
    let view_width = f64::from(view_width);
    let view_height = f64::from(view_height);
    let frame_width = f64::from(frame_width);
    let frame_height = f64::from(frame_height);
    let point_x = f64::from(point_x);
    let point_y = f64::from(point_y);
    let scale = (view_width / frame_width).min(view_height / frame_height);
    let rendered_width = frame_width * scale;
    let rendered_height = frame_height * scale;
    if !scale.is_finite()
        || !rendered_width.is_finite()
        || !rendered_height.is_finite()
        || rendered_width <= 0.0
        || rendered_height <= 0.0
    {
        return None;
    }
    let offset_x = (view_width - rendered_width) / 2.0;
    let offset_y = (view_height - rendered_height) / 2.0;
    let x = (point_x - offset_x) / rendered_width;
    let y = (point_y - offset_y) / rendered_height;
    if !x.is_finite() || !y.is_finite() || !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
        return None;
    }
    Some(DeviceProjectedPoint {
        x: x as f32,
        y: y as f32,
    })
}

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
#[serde(default)]
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
    pub usage: crate::view::usage::UsagePreferences,
    /// How this device presents thread attention and completion events.
    pub notification_mode: crate::view::notifications::NotificationMode,
    /// Whether foreground thread events appear as in-app notices.
    pub in_app_notifications_enabled: bool,
    /// Routes new threads across ready provider instances on this device.
    pub load_balancing_enabled: bool,
    /// Integer weights by provider instance; omitted instances use 100.
    pub load_balancing_weights: BTreeMap<String, u8>,
    /// Device-local screenshot capture behavior.
    pub snapshot_capture: crate::view::snapshot_capture::SnapshotPreferences,
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
            usage: Default::default(),
            notification_mode: crate::view::notifications::NotificationMode::default(),
            in_app_notifications_enabled: true,
            load_balancing_enabled: false,
            load_balancing_weights: BTreeMap::new(),
            snapshot_capture: Default::default(),
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
    /// Service revisions describe discovery/configuration. This revision is
    /// advanced by accepted frame events and stream pruning for native redraws.
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
    closed_recordings: BTreeMap<(String, String, String), (u64, String)>,
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
            session.session_epoch.clone(),
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
            session_epoch: session.session_epoch,
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
        self.duo_controls.remove(&key);
        None
    }

    pub fn complete_duo_for_input(
        &mut self,
        thread_id: &ThreadId,
        input: &agent_protocol::device::DeviceInput,
    ) -> Option<DeviceDuoRequest> {
        if !matches!(input.input, agent_protocol::device::DeviceInputKind::Duo { .. }) {
            return None;
        }
        let effective_host = input
            .host_id
            .as_deref()
            .unwrap_or(agent_protocol::device::LOCAL_DEVICE_HOST_ID);
        let key = self.duo_controls.keys().find(|(thread, host, device, _)| {
            thread == &thread_id.to_string() && host == effective_host && device == &input.device_id
        })?.clone();
        let (command, request_id) = {
            let control = self.duo_controls.get(&key)?;
            (control.requested.clone()?, control.active_request_id?)
        };
        self.complete_duo(
            &DeviceDuoRequest {
                thread_id: thread_id.clone(),
                host_id: input.host_id.clone(),
                device_id: input.device_id.clone(),
                session_epoch: key.3,
                request_id,
                command,
            },
            true,
            None,
        )
    }

    pub fn fail_duo_for_input(
        &mut self,
        thread_id: &ThreadId,
        input: &agent_protocol::device::DeviceInput,
        error: impl Into<String>,
    ) {
        if !matches!(input.input, agent_protocol::device::DeviceInputKind::Duo { .. }) {
            return;
        }
        let effective_host = input
            .host_id
            .as_deref()
            .unwrap_or(agent_protocol::device::LOCAL_DEVICE_HOST_ID);
        let Some(key) = self
            .duo_controls
            .keys()
            .find(|(thread, host, device, _)| {
                thread == &thread_id.to_string() && host == effective_host && device == &input.device_id
            })
            .cloned()
        else {
            return;
        };
        let Some(control) = self.duo_controls.get(&key).cloned() else {
            return;
        };
        let Some(request_id) = control.active_request_id else {
            return;
        };
        self.fail_duo(
            &DeviceDuoRequest {
                thread_id: thread_id.clone(),
                host_id: input.host_id.clone(),
                device_id: input.device_id.clone(),
                session_epoch: key.3,
                request_id,
                command: control
                    .requested
                    .unwrap_or(crate::state::DeviceDuoCommandIntent::Table { value: false }),
            },
            error,
        );
    }

    pub fn fail_duo(&mut self, request: &DeviceDuoRequest, error: impl Into<String>) {
        let _ = self.complete_duo(request, false, Some(error.into()));
    }

    pub fn clear_duo_for_closed_sessions(&mut self) {
        let sessions = self.sessions.clone();
        self.duo_controls.retain(|(thread, host, device, epoch), _| {
            sessions.iter().any(|session| {
                session.thread_id.to_string() == *thread
                    && session.host_id == *host
                    && session.device_id == *device
                    && session.session_epoch == *epoch
            })
        });
    }

    fn accepts_thread_event(&self, thread_id: &agent_domain::ThreadId, host_id: &str, device_id: &str, epoch: &str) -> bool {
        self.sessions.iter().any(|session| {
            &session.thread_id == thread_id
                && session.host_id == host_id
                && session.device_id == device_id
                && session.session_epoch == epoch
        })
    }

    fn accepts_device_event(&self, host_id: &str, device_id: &str, epoch: &str) -> bool {
        self.sessions.iter().any(|session| {
            session.host_id == host_id && session.device_id == device_id && session.session_epoch == epoch
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
                            session.session_epoch.clone(),
                        )
                    })
                    .collect::<std::collections::BTreeSet<_>>();
                self.frames.retain(|key, frame| {
                    let keep = active.contains(&(
                        key.0.clone(),
                        key.1.clone(),
                        key.2.clone(),
                        frame.session_epoch.clone(),
                    ));
                    frame_stream_changed |= !keep;
                    keep
                });
                self.video_frames.retain(|key, frame| {
                    let keep = active.contains(&(
                        key.0.clone(),
                        key.1.clone(),
                        key.2.clone(),
                        frame.session_epoch.clone(),
                    ));
                    frame_stream_changed |= !keep;
                    keep
                });
                self.video_events.retain(|key, events| {
                    events.retain(|frame| {
                        let keep = active.contains(&(
                            key.0.clone(),
                            key.1.clone(),
                            key.2.clone(),
                            frame.session_epoch.clone(),
                        ));
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
                let active_epochs = self.sessions.iter().fold(
                    BTreeMap::<(String, String), BTreeSet<String>>::new(),
                    |mut epochs, session| {
                        epochs
                            .entry((session.host_id.clone(), session.device_id.clone()))
                            .or_default()
                            .insert(session.session_epoch.clone());
                        epochs
                    },
                );
                self.details
                    .retain(|key, _| active_devices.contains(key));
                self.accessibility.retain(|key, tree| {
                    active_devices.contains(key)
                        && active_epochs
                            .get(key)
                            .is_some_and(|epochs| epochs.contains(&tree.session_epoch))
                });
                self.event_log.retain(|key, entries| {
                    if !active_devices.contains(key) {
                        return false;
                    }
                    entries.retain(|entry| {
                        active_epochs
                            .get(key)
                            .is_some_and(|epochs| epochs.contains(&entry.session_epoch))
                    });
                    !entries.is_empty()
                });
                self.foreground.retain(|key, update| {
                    active_devices.contains(key)
                        && active_epochs
                            .get(key)
                            .is_some_and(|epochs| epochs.contains(&update.session_epoch))
                });
                self.screens.retain(|key, screen| {
                    active.contains(&(
                        key.0.clone(),
                        key.1.clone(),
                        key.2.clone(),
                        screen.session_epoch.clone(),
                    ))
                });
                let removed_recordings = self
                    .recordings
                    .iter()
                    .filter_map(|(key, status)| {
                        let keep = active.iter().any(|(active_thread, active_host, active_device, active_epoch)| {
                            active_thread == &key.0
                                && active_host == &key.1
                                && active_device == &key.2
                                && active_epoch == &status.session_epoch
                        });
                        (!keep).then(|| (key.clone(), (status.recording_id, status.session_epoch.clone())))
                    })
                    .collect::<Vec<_>>();
                self.recordings.retain(|(thread, host, device), status| {
                    active.iter().any(|(active_thread, active_host, active_device, active_epoch)| {
                        active_thread == thread
                            && active_host == host
                            && active_device == device
                            && active_epoch == &status.session_epoch
                    })
                });
                for (key, lifetime) in removed_recordings {
                    self.closed_recordings.insert(key, lifetime);
                }
                if self.last_recording.as_ref().is_some_and(|recording| {
                    !active.iter().any(|(thread, host, device, _)| {
                        thread == &recording.status.thread_id.to_string()
                            && host == &recording.status.host_id
                            && device == &recording.status.device_id
                    })
                }) {
                    self.last_recording = None;
                }
                self.clear_duo_for_closed_sessions();
                if frame_stream_changed {
                    self.frame_revision = self.frame_revision.saturating_add(1);
                }
                self.error = None;
            }
            DeviceEvent::Frame(frame) => {
                if !self.accepts_thread_event(&frame.thread_id, &frame.device.host_id, &frame.device.id, &frame.session_epoch) {
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
                if !self.accepts_thread_event(&frame.thread_id, &frame.device.host_id, &frame.device.id, &frame.session_epoch) {
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
                    .is_some_and(|latest| latest.session_epoch != frame.session_epoch)
                {
                    self.video_events.remove(&key);
                }
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
                if !self.accepts_device_event(&tree.host_id, &tree.device_id, &tree.session_epoch) {
                    return;
                }
                self.accessibility
                    .insert((tree.host_id.clone(), tree.device_id.clone()), tree);
            }
            DeviceEvent::EventLog(entry) => {
                if !self.accepts_device_event(&entry.host_id, &entry.device_id, &entry.session_epoch) {
                    return;
                }
                let log = self
                    .event_log
                    .entry((entry.host_id.clone(), entry.device_id.clone()))
                    .or_default();
                if log
                    .first()
                    .is_some_and(|existing| existing.session_epoch != entry.session_epoch)
                {
                    log.clear();
                }
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
                if !self.accepts_device_event(&update.host_id, &update.device_id, &update.session_epoch) {
                    return;
                }
                self.foreground.insert((update.host_id.clone(), update.device_id.clone()), update);
            }
            DeviceEvent::Screen(screen) => {
                if let (Some(thread), Some(host), Some(device)) = (&screen.thread_id, &screen.host_id, &screen.device_id)
                    && self.accepts_thread_event(thread, host, device, &screen.session_epoch)
                {
                    self.screens.insert((thread.to_string(), host.clone(), device.clone(), screen.screen_id.unwrap_or(0)), screen);
                }
            }
            DeviceEvent::Recording(status) => {
                let key = (status.thread_id.to_string(), status.host_id.clone(), status.device_id.clone());
                if status.active {
                    if !self.sessions.iter().any(|session| {
                        session.thread_id == status.thread_id
                            && session.host_id == status.host_id
                            && session.device_id == status.device_id
                            && session.session_epoch == status.session_epoch
                    }) {
                        return;
                    }
                    if self
                        .recordings
                        .get(&key)
                        .is_none_or(|current| status.recording_id >= current.recording_id)
                    {
                        self.closed_recordings.remove(&key);
                        self.recordings.insert(key, status);
                    }
                } else if self
                    .recordings
                    .get(&key)
                    .is_some_and(|current| {
                        current.recording_id == status.recording_id
                            && current.session_epoch == status.session_epoch
                    })
                {
                    self.recordings.remove(&key);
                    self.closed_recordings
                        .insert(key, (status.recording_id, status.session_epoch));
                }
            }
            DeviceEvent::RecordingComplete(recording) => {
                let key = (
                    recording.status.thread_id.to_string(),
                    recording.status.host_id.clone(),
                    recording.status.device_id.clone(),
                );
                let completion_was_pending = if let Some(current) = self.recordings.get(&key) {
                    if current.recording_id != recording.status.recording_id
                        || current.session_epoch != recording.status.session_epoch
                    {
                        return;
                    }
                    self.recordings.remove(&key);
                    false
                } else if self.sessions.iter().any(|session| {
                    session.thread_id.to_string() == key.0
                        && session.host_id == key.1
                        && session.device_id == key.2
                        && session.session_epoch != recording.status.session_epoch
                }) {
                    return;
                } else if self.closed_recordings.get(&key).is_none_or(|(recording_id, session_epoch)| {
                    *recording_id != recording.status.recording_id
                        || session_epoch != &recording.status.session_epoch
                }) {
                    return;
                } else {
                    true
                };
                if completion_was_pending {
                    self.closed_recordings.remove(&key);
                }
                if self
                    .last_recording
                    .as_ref()
                    .is_none_or(|current| {
                        let same_lifetime_key = current.status.thread_id == recording.status.thread_id
                            && current.status.host_id == recording.status.host_id
                            && current.status.device_id == recording.status.device_id;
                        !same_lifetime_key || recording.status.recording_id >= current.status.recording_id
                    })
                {
                    self.last_recording = Some(recording);
                }
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
    use agent_protocol::device::{DeviceFrame, DeviceFrameEncoding, DevicePlatform, DeviceRecording, DeviceRecordingFormat, DeviceRecordingStatus, DeviceScreenConfig, DeviceSummary, DeviceVideoFrame, DeviceOrientation};

    fn session(thread: &str, host: &str, device: &str) -> DeviceSession {
        DeviceSession {
            thread_id: agent_domain::ThreadId::new(thread).unwrap(),
            host_id: host.into(),
            device_id: device.into(),
            platform: DevicePlatform::Android,
            opened_at: "0".into(),
            session_epoch: "0".into(),
        }
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
            session_epoch: current.session_epoch.clone(),
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
            recording_id: 1,
            session_epoch: current.session_epoch.clone(),
            format: DeviceRecordingFormat::Mp4,
            file_name: "device.mp4".into(),
            mime_type: "video/mp4".into(),
            active: true,
            started_at: "0".into(),
            frame_count: 1,
            byte_count: 2,
            error: None,
        };
        state.apply_event(DeviceEvent::Recording(status.clone()));
        assert_eq!(state.recordings.len(), 1);
        state.apply_event(DeviceEvent::RecordingComplete(DeviceRecording {
            status: DeviceRecordingStatus { active: false, ..status },
            bytes: vec![1, 2],
        }));
        assert!(state.recordings.is_empty());
        assert_eq!(state.last_recording.as_ref().unwrap().bytes, vec![1, 2]);
        state.apply_event(DeviceEvent::State(DeviceServiceState::default()));
        assert!(state.recordings.is_empty());
        assert!(state.last_recording.is_none());
    }

    #[test]
    fn screen_metadata_keeps_duo_panel_identities() {
        let current = session("screens", "host", "device");
        let mut state = DeviceState::default();
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![current.clone()],
            ..DeviceServiceState::default()
        }));
        for screen_id in [Some(1), Some(3)] {
            state.apply_event(DeviceEvent::Screen(DeviceScreenConfig {
                thread_id: Some(current.thread_id.clone()),
                session_epoch: current.session_epoch.clone(),
                host_id: Some(current.host_id.clone()),
                device_id: Some(current.device_id.clone()),
                width: 100,
                height: 100,
                orientation: DeviceOrientation::Portrait,
                screen_id,
                supports_hinge_angle: true,
                supports_physical_orientation: false,
                hinge_angle: Some(90.0),
                hinge_pose: Some("book".into()),
                table_mode: false,
                table_mode_available: true,
            }));
        }
        assert_eq!(state.screens.len(), 2);
        let view = crate::view::device::device_view(&Snapshot { device: state, ..Snapshot::default() });
        assert_eq!(view.screens.iter().map(|screen| screen.screen_id).collect::<Vec<_>>(), vec![Some(1), Some(3)]);
    }

    #[test]
    fn ordered_video_events_are_bounded_and_reject_late_sequences() {
        let current = session("video-thread", "host", "device");
        let mut state = DeviceState::default();
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![current.clone()],
            ..DeviceServiceState::default()
        }));
        let frame = |sequence| DeviceVideoFrame {
            thread_id: current.thread_id.clone(),
            session_epoch: current.session_epoch.clone(),
            device: DeviceSummary {
                host_id: current.host_id.clone(),
                id: current.device_id.clone(),
                platform: current.platform,
                name: "Pixel".into(),
                version: "Android".into(),
                booted: true,
                physical: false,
            },
            payload: vec![0, 0, 1, sequence as u8],
            encoding: DeviceFrameEncoding::H264,
            width: 2,
            height: 2,
            sequence,
            timestamp_us: Some(sequence),
            keyframe: sequence == 1,
            screen_id: Some(0),
        };
        for sequence in 1..=(MAX_VIDEO_EVENTS_PER_STREAM as u64 + 2) {
            state.apply_event(DeviceEvent::Video(frame(sequence)));
        }
        {
            let events = state.video_events.values().next().unwrap();
            assert_eq!(events.len(), MAX_VIDEO_EVENTS_PER_STREAM);
            assert_eq!(events.front().unwrap().sequence, 1);
            assert_eq!(events.back().unwrap().sequence, MAX_VIDEO_EVENTS_PER_STREAM as u64 + 2);
        }
        state.apply_event(DeviceEvent::Video(frame(2)));
        assert_eq!(
            state.video_events.values().next().unwrap().back().unwrap().sequence,
            MAX_VIDEO_EVENTS_PER_STREAM as u64 + 2
        );
    }

    #[test]
    fn late_video_events_are_rejected_after_session_close() {
        let current = session("closed-video", "host", "device");
        let mut state = DeviceState::default();
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![current.clone()],
            ..DeviceServiceState::default()
        }));
        let frame = DeviceVideoFrame {
            thread_id: current.thread_id.clone(),
            session_epoch: current.session_epoch.clone(),
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
        state.apply_event(DeviceEvent::State(DeviceServiceState::default()));
        state.apply_event(DeviceEvent::Video(frame));
        assert!(state.video_events.is_empty());
    }

    #[test]
    fn reopened_session_drops_previous_video_epoch() {
        let current = session("epoch-video", "host", "device");
        let mut state = DeviceState::default();
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![current.clone()],
            ..DeviceServiceState::default()
        }));
        let frame = DeviceVideoFrame {
            thread_id: current.thread_id.clone(),
            session_epoch: current.session_epoch.clone(),
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
            sequence: 99,
            timestamp_us: Some(99),
            keyframe: true,
            screen_id: Some(0),
        };
        state.apply_event(DeviceEvent::Video(frame));
        let mut reopened = current.clone();
        reopened.opened_at = "1".into();
        reopened.session_epoch = "1".into();
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![reopened.clone()],
            ..DeviceServiceState::default()
        }));
        assert!(state.video_events.is_empty());
        assert!(state.video_frames.is_empty());

        state.apply_event(DeviceEvent::Accessibility(DeviceAccessibilityTree {
            host_id: current.host_id.clone(),
            device_id: current.device_id.clone(),
            session_epoch: current.session_epoch.clone(),
            elements: vec![],
            errors: vec![],
            read_at: "stale".into(),
        }));
        state.apply_event(DeviceEvent::EventLog(DeviceEventLogEntry {
            host_id: current.host_id.clone(),
            device_id: current.device_id.clone(),
            session_epoch: current.session_epoch,
            id: 2,
            timestamp: "stale".into(),
            kind: "stale".into(),
            summary: "stale".into(),
        }));
        assert!(state.accessibility.is_empty());
        assert!(state.event_log.is_empty());
        assert!(state.accepts_device_event(&reopened.host_id, &reopened.device_id, &reopened.session_epoch));
        let next = DeviceVideoFrame {
            thread_id: reopened.thread_id,
            session_epoch: reopened.session_epoch,
            device: DeviceSummary {
                host_id: reopened.host_id,
                id: reopened.device_id,
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
            sequence: 1,
            timestamp_us: Some(1),
            keyframe: true,
            screen_id: Some(0),
        };
        state.apply_event(DeviceEvent::Video(next));
        assert_eq!(state.video_events.values().next().unwrap().len(), 1);
    }

    #[test]
    fn recording_completion_survives_session_removal() {
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
            recording_id: 1,
            session_epoch: current.session_epoch.clone(),
            format: DeviceRecordingFormat::Mp4,
            file_name: "device.mp4".into(),
            mime_type: "video/mp4".into(),
            active: true,
            started_at: "0".into(),
            frame_count: 1,
            byte_count: 2,
            error: None,
        };
        state.apply_event(DeviceEvent::Recording(status.clone()));
        state.apply_event(DeviceEvent::State(DeviceServiceState::default()));
        state.apply_event(DeviceEvent::RecordingComplete(DeviceRecording {
            status: DeviceRecordingStatus { active: false, ..status },
            bytes: vec![1, 2],
        }));
        assert_eq!(state.last_recording.as_ref().unwrap().bytes, vec![1, 2]);
    }

    #[test]
    fn late_completion_cannot_remove_a_new_recording_lifetime() {
        let old = session("thread", "host", "device");
        let mut state = DeviceState::default();
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![old.clone()],
            ..DeviceServiceState::default()
        }));
        let old_status = DeviceRecordingStatus {
            thread_id: old.thread_id.clone(),
            host_id: old.host_id.clone(),
            device_id: old.device_id.clone(),
            recording_id: 1,
            session_epoch: old.session_epoch.clone(),
            format: DeviceRecordingFormat::Mp4,
            file_name: "old.mp4".into(),
            mime_type: "video/mp4".into(),
            active: true,
            started_at: "old".into(),
            frame_count: 1,
            byte_count: 1,
            error: None,
        };
        state.apply_event(DeviceEvent::Recording(old_status.clone()));

        let current = DeviceSession { session_epoch: "new".into(), ..old.clone() };
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![current.clone()],
            ..DeviceServiceState::default()
        }));
        let new_status = DeviceRecordingStatus {
            recording_id: 2,
            session_epoch: current.session_epoch.clone(),
            file_name: "new.mp4".into(),
            ..old_status.clone()
        };
        state.apply_event(DeviceEvent::Recording(new_status.clone()));
        state.apply_event(DeviceEvent::RecordingComplete(DeviceRecording {
            status: DeviceRecordingStatus { active: false, ..old_status },
            bytes: vec![1],
        }));
        assert_eq!(state.recordings.get(&("thread".into(), "host".into(), "device".into())).map(|status| status.recording_id), Some(2));
        assert_eq!(state.last_recording.as_ref().map(|recording| recording.bytes.clone()), Some(vec![1]));

        state.apply_event(DeviceEvent::RecordingComplete(DeviceRecording {
            status: DeviceRecordingStatus { active: false, ..new_status },
            bytes: vec![2],
        }));
        assert!(state.recordings.is_empty());
        assert_eq!(state.last_recording.as_ref().map(|recording| recording.bytes.clone()), Some(vec![2]));
    }

    #[test]
    fn duo_controls_are_single_flight_and_epoch_scoped() {
        let current = session("duo", "host", "device");
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
                crate::state::DeviceDuoCommandIntent::Table { value: true },
            )
            .unwrap()
            .expect("first Duo request is sent immediately");
        assert!(state
            .enqueue_duo(
                current.thread_id.clone(),
                Some(current.host_id.clone()),
                current.device_id.clone(),
                crate::state::DeviceDuoCommandIntent::Angle { value: 30.0 },
            )
            .unwrap()
            .is_none());

        let next = state
            .complete_duo(&first, true, None)
            .expect("queued Duo request is promoted after completion");
        assert_eq!(
            next.command,
            crate::state::DeviceDuoCommandIntent::Angle { value: 30.0 }
        );
        assert!(state.complete_duo(&next, true, None).is_none());
        assert!(state.duo_controls.is_empty());

        let reopened = DeviceSession {
            session_epoch: "new".into(),
            ..current
        };
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![reopened],
            ..DeviceServiceState::default()
        }));
        assert!(state.duo_controls.is_empty());
    }

    #[test]
    fn touch_projection_keeps_extreme_finite_dimensions_valid() {
        let tiny = f32::from_bits(1);
        let projected = project_device_point(
            tiny,
            tiny,
            f32::MAX,
            f32::MAX,
            tiny,
            tiny,
        )
        .expect("finite dimensions should produce a finite fit");
        assert!(projected.x.is_finite() && projected.y.is_finite());
        assert!((0.0..=1.0).contains(&projected.x));
        assert!((0.0..=1.0).contains(&projected.y));
    }
}
