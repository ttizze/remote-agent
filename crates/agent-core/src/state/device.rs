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
use std::collections::{BTreeMap, BTreeSet, VecDeque};

const MAX_VIDEO_EVENTS_PER_STREAM: usize = 32;

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
    pub screens: BTreeMap<(String, String, String), DeviceScreenConfig>,
    pub recordings: BTreeMap<(String, String, String), agent_protocol::device::DeviceRecordingStatus>,
    pub last_recording: Option<DeviceRecording>,
    pub last_screenshot: Option<DeviceScreenshot>,
    pub error: Option<String>,
}

impl DeviceState {
    pub fn apply_event(&mut self, event: DeviceEvent) {
        match event {
            DeviceEvent::State(service) => {
                let reopened = self
                    .sessions
                    .iter()
                    .filter_map(|previous| {
                        service
                            .sessions
                            .iter()
                            .find(|current| {
                                current.thread_id == previous.thread_id
                                    && current.host_id == previous.host_id
                                    && current.device_id == previous.device_id
                            })
                            .filter(|current| current.opened_at != previous.opened_at)
                            .map(|_| {
                                (
                                    previous.thread_id.to_string(),
                                    previous.host_id.clone(),
                                    previous.device_id.clone(),
                                )
                            })
                    })
                    .collect::<std::collections::BTreeSet<_>>();
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
                self.frames.retain(|key, _| active.contains(key));
                self.video_frames.retain(|key, _| {
                    active.contains(&(key.0.clone(), key.1.clone(), key.2.clone()))
                });
                self.video_events.retain(|key, _| {
                    active.contains(&(key.0.clone(), key.1.clone(), key.2.clone()))
                });
                self.frames
                    .retain(|key, _| !reopened.contains(key));
                self.video_frames
                    .retain(|key, _| !reopened.contains(&(key.0.clone(), key.1.clone(), key.2.clone())));
                self.video_events
                    .retain(|key, _| !reopened.contains(&(key.0.clone(), key.1.clone(), key.2.clone())));
                let active_devices = self
                    .sessions
                    .iter()
                    .map(|session| (session.host_id.clone(), session.device_id.clone()))
                    .collect::<std::collections::BTreeSet<_>>();
                self.details
                    .retain(|key, _| active_devices.contains(key));
                self.accessibility.retain(|key, _| active_devices.contains(key));
                self.event_log.retain(|key, _| active_devices.contains(key));
                self.foreground.retain(|key, _| active_devices.contains(key));
                self.screens.retain(|key, _| active.contains(key));
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
                if !self.sessions.iter().any(|session| {
                    session.thread_id == frame.thread_id
                        && session.host_id == frame.device.host_id
                        && session.device_id == frame.device.id
                }) {
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
                if !self.sessions.iter().any(|session| {
                    session.thread_id == frame.thread_id
                        && session.host_id == frame.device.host_id
                        && session.device_id == frame.device.id
                }) {
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
                retain_video_tail(events);
            }
            DeviceEvent::Accessibility(tree) => {
                if !self.sessions.iter().any(|session| {
                    session.host_id == tree.host_id && session.device_id == tree.device_id
                }) {
                    return;
                }
                self.accessibility
                    .insert((tree.host_id.clone(), tree.device_id.clone()), tree);
            }
            DeviceEvent::EventLog(entry) => {
                if !self.sessions.iter().any(|session| {
                    session.host_id == entry.host_id && session.device_id == entry.device_id
                }) {
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
                if !self.sessions.iter().any(|session| {
                    session.host_id == update.host_id && session.device_id == update.device_id
                }) {
                    return;
                }
                self.foreground.insert((update.host_id.clone(), update.device_id.clone()), update);
            }
            DeviceEvent::Screen(screen) => {
                if let (Some(thread), Some(host), Some(device)) = (&screen.thread_id, &screen.host_id, &screen.device_id) {
                    if !self.sessions.iter().any(|session| {
                        &session.thread_id == thread
                            && &session.host_id == host
                            && &session.device_id == device
                    }) {
                        return;
                    }
                    self.screens.insert((thread.to_string(), host.clone(), device.clone()), screen);
                }
            }
            DeviceEvent::Recording(status) => {
                let key = (status.thread_id.to_string(), status.host_id.clone(), status.device_id.clone());
                if !self.sessions.iter().any(|session| {
                    session.thread_id == status.thread_id
                        && session.host_id == status.host_id
                        && session.device_id == status.device_id
                }) {
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
                if !self.sessions.iter().any(|session| {
                    session.thread_id == recording.status.thread_id
                        && session.host_id == recording.status.host_id
                        && session.device_id == recording.status.device_id
                }) {
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

fn retain_video_tail(events: &mut VecDeque<DeviceVideoFrame>) {
    while events.len() > MAX_VIDEO_EVENTS_PER_STREAM {
        let latest_keyframe = events.iter().rposition(|frame| frame.keyframe);
        match latest_keyframe {
            Some(index) if index > 0 => {
                for _ in 0..index {
                    events.pop_front();
                }
            }
            Some(_) if events.len() > 1 => {
                events.remove(1);
            }
            _ => {
                events.pop_front();
            }
        }
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
    fn state_updates_prune_frames_for_closed_sessions() {
        let current = session("thread", "host", "device");
        let mut state = DeviceState::default();
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![current.clone()],
            ..DeviceServiceState::default()
        }));
        state.apply_event(DeviceEvent::Frame(DeviceFrame {
            thread_id: current.thread_id.clone(),
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
    fn ordered_video_events_are_bounded_and_reject_late_sequences() {
        let current = session("video-thread", "host", "device");
        let mut state = DeviceState::default();
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![current.clone()],
            ..DeviceServiceState::default()
        }));
        let frame = |sequence| DeviceVideoFrame {
            thread_id: current.thread_id.clone(),
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
        let mut reopened = current;
        reopened.opened_at = "1".into();
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![reopened.clone()],
            ..DeviceServiceState::default()
        }));
        assert!(state.video_events.is_empty());
        assert!(state.video_frames.is_empty());

        let next = DeviceVideoFrame {
            thread_id: reopened.thread_id,
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
}
