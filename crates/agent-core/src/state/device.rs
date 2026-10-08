//! Device state the views read beside the Host's: preferences, holds over the
//! list, answer drafts, panel selections and flows in progress.
use crate::view::projects::import::{ImportToast, SessionImportProgress};
use crate::view::{
    models::ordering::FavoriteModel, requests::QuestionDraft, thread_order::PendingThreadOrder,
    time::TimestampFormat,
};
use agent_domain::CommandId;
use agent_protocol::conversation::SessionScan;
use agent_protocol::device::{DeviceAccessibilityTree, DeviceDetail, DeviceEvent, DeviceEventLogEntry, DeviceForegroundUpdate, DeviceFrame, DeviceRecording, DeviceScreenshot, DeviceScreenConfig, DeviceServiceState, DeviceSession, DeviceVideoFrame};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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
    /// Routes new threads across matching ready environments on this device.
    pub load_balancing_enabled: bool,
    /// Integer weights by environment; omitted environments use the Normal
    /// preference (50).
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
    pub sessions: Vec<DeviceSession>,
    pub details: BTreeMap<(String, String), DeviceDetail>,
    pub frames: BTreeMap<(String, String, String), DeviceFrame>,
    pub video_frames: BTreeMap<(String, String, String, u8), DeviceVideoFrame>,
    pub accessibility: BTreeMap<(String, String), DeviceAccessibilityTree>,
    pub event_log: BTreeMap<(String, String), Vec<DeviceEventLogEntry>>,
    pub foreground: BTreeMap<(String, String), DeviceForegroundUpdate>,
    pub screens: BTreeMap<(String, String, String, u8), DeviceScreenConfig>,
    pub recordings: BTreeMap<(String, String, String), agent_protocol::device::DeviceRecordingStatus>,
    pub last_recording: Option<DeviceRecording>,
    pub last_screenshot: Option<DeviceScreenshot>,
    pub error: Option<String>,
}

impl DeviceState {
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
                let active = self
                    .sessions
                    .iter()
                    .map(|session| {
                        (
                            session.thread_id.to_string(),
                            session.host_id.clone(),
                            session.device_id.clone(),
                        )
                    })
                    .collect::<std::collections::BTreeSet<_>>();
                self.frames.retain(|key, frame| {
                    active.contains(key)
                        && self.sessions.iter().any(|session| {
                            session.thread_id.to_string() == key.0
                                && session.host_id == key.1
                                && session.device_id == key.2
                                && session.session_epoch == frame.session_epoch
                        })
                });
                self.video_frames.retain(|key, frame| {
                    active.contains(&(key.0.clone(), key.1.clone(), key.2.clone()))
                        && self.sessions.iter().any(|session| {
                            session.thread_id.to_string() == key.0
                                && session.host_id == key.1
                                && session.device_id == key.2
                                && session.session_epoch == frame.session_epoch
                        })
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
                    active.contains(&(key.0.clone(), key.1.clone(), key.2.clone()))
                        && self.sessions.iter().any(|session| {
                            session.thread_id.to_string() == key.0
                                && session.host_id == key.1
                                && session.device_id == key.2
                                && session.session_epoch == screen.session_epoch
                        })
                });
                self.recordings.retain(|(thread, host, device), _| active.contains(&(thread.clone(), host.clone(), device.clone())));
                if self.last_recording.as_ref().is_some_and(|recording| {
                    !active.contains(&(
                        recording.status.thread_id.to_string(),
                        recording.status.host_id.clone(),
                        recording.status.device_id.clone(),
                    ))
                }) {
                    self.last_recording = None;
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
            }
            DeviceEvent::Video(frame) => {
                if !self.accepts_thread_event(&frame.thread_id, &frame.device.host_id, &frame.device.id, &frame.session_epoch) {
                    return;
                }
                self.video_frames.insert(
                    (
                        frame.thread_id.to_string(),
                        frame.device.host_id.clone(),
                        frame.device.id.clone(),
                        frame.screen_id.unwrap_or(0),
                    ),
                    frame,
                );
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
    use agent_protocol::device::{DeviceAccessibilityTree, DeviceEventLogEntry, DeviceForegroundUpdate, DeviceFrame, DevicePlatform, DeviceRecording, DeviceRecordingFormat, DeviceRecordingStatus, DeviceScreenConfig, DeviceSummary};

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
        state.apply_event(DeviceEvent::RecordingComplete(DeviceRecording { status: DeviceRecordingStatus { active: false, ..status }, bytes: vec![1, 2] }));
        assert!(state.recordings.is_empty());
        assert_eq!(state.last_recording.as_ref().unwrap().bytes, vec![1, 2]);
        state.apply_event(DeviceEvent::State(DeviceServiceState::default()));
        assert!(state.recordings.is_empty());
        assert!(state.last_recording.is_none());
    }

    #[test]
    fn reopening_a_session_prunes_and_rejects_old_device_metadata() {
        let current = session("thread", "host", "device");
        let mut state = DeviceState::default();
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![current.clone()],
            ..DeviceServiceState::default()
        }));
        state.apply_event(DeviceEvent::Accessibility(DeviceAccessibilityTree {
            host_id: current.host_id.clone(),
            device_id: current.device_id.clone(),
            session_epoch: current.session_epoch.clone(),
            elements: vec![],
            errors: vec![],
            read_at: "old".into(),
        }));
        state.apply_event(DeviceEvent::Foreground(DeviceForegroundUpdate {
            host_id: current.host_id.clone(),
            device_id: current.device_id.clone(),
            session_epoch: current.session_epoch.clone(),
            app: None,
            received_at: "old".into(),
        }));
        state.apply_event(DeviceEvent::EventLog(DeviceEventLogEntry {
            host_id: current.host_id.clone(),
            device_id: current.device_id.clone(),
            session_epoch: current.session_epoch.clone(),
            id: 1,
            timestamp: "old".into(),
            kind: "old".into(),
            summary: "old".into(),
        }));
        state.apply_event(DeviceEvent::Screen(DeviceScreenConfig {
            thread_id: Some(current.thread_id.clone()),
            session_epoch: current.session_epoch.clone(),
            host_id: Some(current.host_id.clone()),
            device_id: Some(current.device_id.clone()),
            width: 1,
            height: 1,
            orientation: agent_protocol::device::DeviceOrientation::Portrait,
            screen_id: None,
            supports_hinge_angle: false,
            supports_physical_orientation: false,
            hinge_angle: None,
            hinge_pose: None,
            table_mode: false,
            table_mode_available: false,
        }));

        let reopened = DeviceSession { session_epoch: "new".into(), ..current.clone() };
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![reopened.clone()],
            ..DeviceServiceState::default()
        }));
        assert!(state.accessibility.is_empty());
        assert!(state.foreground.is_empty());
        assert!(state.event_log.is_empty());
        assert!(state.screens.is_empty());

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
}
