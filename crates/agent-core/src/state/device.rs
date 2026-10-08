//! Device state the views read beside the Host's: preferences, holds over the
//! list, answer drafts, panel selections and flows in progress.
use crate::view::projects::import::{ImportToast, SessionImportProgress};
use crate::view::{
    models::ordering::FavoriteModel, requests::QuestionDraft, thread_order::PendingThreadOrder,
    time::TimestampFormat,
};
use agent_domain::CommandId;
use agent_protocol::conversation::SessionScan;
use agent_protocol::device::{DeviceDetail, DeviceEvent, DeviceFrame, DeviceScreenshot, DeviceServiceState, DeviceSession};
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
    pub last_screenshot: Option<DeviceScreenshot>,
    pub error: Option<String>,
}

impl DeviceState {
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
                self.frames.retain(|key, _| active.contains(key));
                let active_devices = self
                    .sessions
                    .iter()
                    .map(|session| (session.host_id.clone(), session.device_id.clone()))
                    .collect::<std::collections::BTreeSet<_>>();
                self.details
                    .retain(|key, _| active_devices.contains(key));
                self.error = None;
            }
            DeviceEvent::Frame(frame) => {
                self.frames.insert(
                    (
                        frame.thread_id.to_string(),
                        frame.device.host_id.clone(),
                        frame.device.id.clone(),
                    ),
                    frame,
                );
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
    use agent_protocol::device::{DeviceFrame, DevicePlatform, DeviceSummary};

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
}
