//! Host owned Preview sessions and local development server discovery.
pub(crate) mod ports;

use agent_protocol::preview::{
    normalize_preview_url, PreviewAppearance, PreviewEvent, PreviewListResult, PreviewNavStatus,
    PreviewRecordingStatus, PreviewSessionSnapshot, PreviewViewportSetting, PreviewZoom,
    validate_profile_id,
};
use agent_domain::ThreadId;
use std::{collections::BTreeMap, sync::Mutex};

pub(crate) use ports::PortScanner;

#[derive(Default)]
struct State {
    sessions: BTreeMap<(ThreadId, String), PreviewSessionSnapshot>,
    recordings: BTreeMap<(ThreadId, String), PreviewRecordingStatus>,
    invalidated_recordings: BTreeMap<(ThreadId, String), ()>,
    revision: u64,
}

/// Metadata owner for browser tabs used by the Preview surface.  The browser
/// resource owns pixels and input; this owner gives reconnecting clients a
/// stable session view and ordered revisions.
pub(crate) struct PreviewManager {
    state: Mutex<State>,
    server_epoch: String,
    events: tokio::sync::broadcast::Sender<PreviewEvent>,
}

impl Default for PreviewManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PreviewManager {
    pub fn new() -> Self {
        let (events, _) = tokio::sync::broadcast::channel(128);
        Self {
            state: Mutex::new(State::default()),
            server_epoch: uuid::Uuid::new_v4().to_string(),
            events,
        }
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<PreviewEvent> {
        self.events.subscribe()
    }

    pub fn open(
        &self,
        thread_id: ThreadId,
        tab_id: String,
        url: Option<&str>,
        viewport: PreviewViewportSetting,
        appearance: PreviewAppearance,
        zoom: PreviewZoom,
        profile_id: Option<String>,
    ) -> Result<PreviewSessionSnapshot, String> {
        viewport.validate()?;
        if let Some(profile_id) = &profile_id {
            validate_profile_id(profile_id)?;
        }
        let nav_status = match url {
            Some(url) => PreviewNavStatus::Loading {
                url: normalize_preview_url(url)?,
                title: String::new(),
            },
            None => PreviewNavStatus::Idle,
        };
        let snapshot = PreviewSessionSnapshot {
            thread_id: thread_id.clone(),
            tab_id: tab_id.clone(),
            nav_status,
            can_go_back: false,
            can_go_forward: false,
            viewport,
            zoom,
            appearance,
            profile_id,
            updated_at: now(),
        };
        snapshot.validate()?;
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.revision = state.revision.saturating_add(1);
        state.sessions.insert((thread_id.clone(), tab_id.clone()), snapshot.clone());
        let _ = self.events.send(PreviewEvent::Opened {
            thread_id,
            tab_id,
            revision: state.revision,
            server_epoch: self.server_epoch.clone(),
            created_at: snapshot.updated_at.clone(),
            snapshot: snapshot.clone(),
        });
        Ok(snapshot)
    }

    pub fn list(&self, thread_id: &ThreadId) -> PreviewListResult {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let mut sessions: Vec<_> = state
                .sessions
                .iter()
                .filter(|((thread, _), _)| thread == thread_id)
                .map(|(_, snapshot)| snapshot.clone())
                .collect();
        sessions.sort_by(|left, right| left.updated_at.cmp(&right.updated_at));
        PreviewListResult {
            sessions,
            recordings: state
                .recordings
                .iter()
                .filter(|((thread, _), _)| thread == thread_id)
                .map(|(_, status)| status.clone())
                .collect(),
            invalidated_recordings: state
                .invalidated_recordings
                .keys()
                .filter(|(thread, _)| thread == thread_id)
                .map(|(_, tab_id)| tab_id.clone())
                .collect(),
            local_servers: Vec::new(),
            scanned_at: now(),
            server_epoch: self.server_epoch.clone(),
            revision: state.revision,
            scanner_epoch: String::new(),
            scanner_revision: 0,
        }
    }

    pub fn get(&self, thread_id: &ThreadId, tab_id: &str) -> Result<PreviewSessionSnapshot, String> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .sessions
            .get(&(thread_id.clone(), tab_id.to_owned()))
            .cloned()
            .ok_or_else(|| "preview session was not found".into())
    }

    pub fn navigate(&self, thread_id: &ThreadId, tab_id: &str, url: &str) -> Result<PreviewSessionSnapshot, String> {
        let url = normalize_preview_url(url)?;
        self.update(thread_id, tab_id, false, |snapshot| {
            let title = match &snapshot.nav_status {
                PreviewNavStatus::Idle => String::new(),
                PreviewNavStatus::Loading { title, .. }
                | PreviewNavStatus::Success { title, .. }
                | PreviewNavStatus::LoadFailed { title, .. } => title.clone(),
            };
            snapshot.nav_status = PreviewNavStatus::Success { url, title };
        })
    }

    pub fn report_status(
        &self,
        thread_id: &ThreadId,
        tab_id: &str,
        nav_status: PreviewNavStatus,
        can_go_back: bool,
        can_go_forward: bool,
    ) -> Result<(), String> {
        nav_status.validate()?;
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        {
            let current = state
                .sessions
                .get(&(thread_id.clone(), tab_id.to_owned()))
                .ok_or_else(|| "preview session was not found".to_owned())?;
            if current.nav_status == nav_status
                && current.can_go_back == can_go_back
                && current.can_go_forward == can_go_forward
            {
                return Ok(());
            }
        }
        let snapshot = {
            let snapshot = state
                .sessions
                .get_mut(&(thread_id.clone(), tab_id.to_owned()))
                .ok_or_else(|| "preview session was not found".to_owned())?;
            snapshot.nav_status = nav_status;
            snapshot.can_go_back = can_go_back;
            snapshot.can_go_forward = can_go_forward;
            snapshot.updated_at = now();
            snapshot.clone()
        };
        state.revision = state.revision.saturating_add(1);
        let event = match &snapshot.nav_status {
            PreviewNavStatus::LoadFailed {
                url,
                title,
                code,
                description,
            } => PreviewEvent::Failed {
                thread_id: thread_id.clone(),
                tab_id: tab_id.to_owned(),
                revision: state.revision,
                server_epoch: self.server_epoch.clone(),
                created_at: snapshot.updated_at.clone(),
                url: url.clone(),
                title: title.clone(),
                code: *code,
                description: description.clone(),
            },
            _ => PreviewEvent::Navigated {
                thread_id: thread_id.clone(),
                tab_id: tab_id.to_owned(),
                revision: state.revision,
                server_epoch: self.server_epoch.clone(),
                created_at: snapshot.updated_at.clone(),
                snapshot,
            },
        };
        let _ = self.events.send(event);
        Ok(())
    }

    pub fn refresh(&self, thread_id: &ThreadId, tab_id: &str) -> Result<(), String> {
        self.get(thread_id, tab_id).map(|_| ())
    }

    pub fn resize(&self, thread_id: &ThreadId, tab_id: &str, viewport: PreviewViewportSetting) -> Result<PreviewSessionSnapshot, String> {
        viewport.validate()?;
        self.update(thread_id, tab_id, true, |snapshot| snapshot.viewport = viewport)
    }

    pub fn appearance(&self, thread_id: &ThreadId, tab_id: &str, appearance: PreviewAppearance) -> Result<PreviewSessionSnapshot, String> {
        self.update(thread_id, tab_id, false, |snapshot| snapshot.appearance = appearance)
    }

    pub fn zoom(&self, thread_id: &ThreadId, tab_id: &str, zoom: PreviewZoom) -> Result<PreviewSessionSnapshot, String> {
        self.update(thread_id, tab_id, false, |snapshot| snapshot.zoom = zoom)
    }

    pub fn recording_started(
        &self,
        thread_id: ThreadId,
        status: PreviewRecordingStatus,
    ) -> Result<(), String> {
        if status.tab_id.trim().is_empty() {
            return Err("preview recording tab id is invalid".into());
        }
        if status.recording_id.trim().is_empty() {
            return Err("preview recording id is invalid".into());
        }
        let tab_id = status.tab_id.clone();
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if !state
            .sessions
            .contains_key(&(thread_id.clone(), tab_id.clone()))
        {
            return Err("preview session was not found".into());
        }
        state
            .recordings
            .insert((thread_id.clone(), tab_id.clone()), status.clone());
        state
            .invalidated_recordings
            .remove(&(thread_id.clone(), tab_id.clone()));
        state.revision = state.revision.saturating_add(1);
        let _ = self.events.send(PreviewEvent::RecordingChanged {
            thread_id,
            tab_id,
            revision: state.revision,
            server_epoch: self.server_epoch.clone(),
            created_at: now(),
            status,
        });
        Ok(())
    }

    pub fn recording_finished(&self, thread_id: &ThreadId, tab_id: &str, recording_id: &str) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let key = (thread_id.clone(), tab_id.to_owned());
        let Some(active) = state.recordings.get(&key) else {
            // A close can remove the session after the recording task has
            // published its completion but before its monitor publishes this
            // final status. Do not resurrect a recording for a closed tab.
            return;
        };
        if active.recording_id != recording_id {
            // A detached or otherwise late monitor must not finish a newer
            // capture that reused the same tab.
            return;
        }
        state.recordings.remove(&key);
        state.revision = state.revision.saturating_add(1);
        let status = PreviewRecordingStatus {
            tab_id: tab_id.to_owned(),
            recording_id: recording_id.to_owned(),
            recording: false,
            started_at: None,
        };
        let _ = self.events.send(PreviewEvent::RecordingChanged {
            thread_id: thread_id.clone(),
            tab_id: tab_id.to_owned(),
            revision: state.revision,
            server_epoch: self.server_epoch.clone(),
            created_at: now(),
            status,
        });
    }

    pub fn recording_artifact_removed(&self, thread_id: &ThreadId, tab_id: &str) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if !state
            .sessions
            .contains_key(&(thread_id.clone(), tab_id.to_owned()))
        {
            return;
        }
        if state
            .invalidated_recordings
            .insert((thread_id.clone(), tab_id.to_owned()), ())
            .is_some()
        {
            return;
        }
        state.revision = state.revision.saturating_add(1);
        let _ = self.events.send(PreviewEvent::RecordingArtifactRemoved {
            thread_id: thread_id.clone(),
            tab_id: tab_id.to_owned(),
            revision: state.revision,
            server_epoch: self.server_epoch.clone(),
            created_at: now(),
        });
    }

    pub fn close(&self, thread_id: &ThreadId, tab_id: Option<&str>) -> Vec<String> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let ids: Vec<String> = state
            .sessions
            .keys()
            .filter(|(thread, id)| thread == thread_id && tab_id.is_none_or(|wanted| wanted == id))
            .map(|(_, id)| id.clone())
            .collect();
        for id in &ids {
            state.sessions.remove(&(thread_id.clone(), id.clone()));
            state.recordings.remove(&(thread_id.clone(), id.clone()));
            state
                .invalidated_recordings
                .remove(&(thread_id.clone(), id.clone()));
            state.revision = state.revision.saturating_add(1);
            let _ = self.events.send(PreviewEvent::Closed {
                thread_id: thread_id.clone(),
                tab_id: id.clone(),
                revision: state.revision,
                server_epoch: self.server_epoch.clone(),
                created_at: now(),
            });
        }
        ids
    }

    fn update(
        &self,
        thread_id: &ThreadId,
        tab_id: &str,
        resized: bool,
        change: impl FnOnce(&mut PreviewSessionSnapshot),
    ) -> Result<PreviewSessionSnapshot, String> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let snapshot = {
            let snapshot = state
                .sessions
                .get_mut(&(thread_id.clone(), tab_id.to_owned()))
                .ok_or_else(|| "preview session was not found".to_owned())?;
            change(snapshot);
            snapshot.updated_at = now();
            snapshot.clone()
        };
        state.revision = state.revision.saturating_add(1);
        let event = if resized {
            PreviewEvent::Resized {
                thread_id: thread_id.clone(),
                tab_id: tab_id.to_owned(),
                revision: state.revision,
                server_epoch: self.server_epoch.clone(),
                created_at: snapshot.updated_at.clone(),
                snapshot: snapshot.clone(),
            }
        } else {
            PreviewEvent::Navigated {
                thread_id: thread_id.clone(),
                tab_id: tab_id.to_owned(),
                revision: state.revision,
                server_epoch: self.server_epoch.clone(),
                created_at: snapshot.updated_at.clone(),
                snapshot: snapshot.clone(),
            }
        };
        let _ = self.events.send(event);
        Ok(snapshot)
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(value: &str) -> ThreadId { ThreadId::new(value).unwrap() }

    #[test]
    fn revisions_are_monotonic_and_list_is_thread_scoped() {
        let manager = PreviewManager::new();
        let first = manager.open(thread("one"), "tab-a".into(), None, PreviewViewportSetting::Fill, PreviewAppearance::System, PreviewZoom::X100, None).unwrap();
        manager.open(thread("two"), "tab-b".into(), None, PreviewViewportSetting::Fill, PreviewAppearance::System, PreviewZoom::X100, None).unwrap();
        assert_eq!(manager.list(&thread("one")).sessions, vec![first.clone()]);
        let before = manager.list(&thread("one")).revision;
        manager.resize(&thread("one"), "tab-a", PreviewViewportSetting::Freeform { width: 390, height: 844 }).unwrap();
        assert!(manager.list(&thread("one")).revision > before);
    }

    #[test]
    fn list_exposes_a_stable_nonempty_server_epoch() {
        let manager = PreviewManager::new();
        let first = manager.list(&thread("one"));
        let second = manager.list(&thread("one"));
        assert!(!first.server_epoch.is_empty());
        assert_eq!(first.server_epoch, second.server_epoch);
    }

    #[test]
    fn closing_one_tab_keeps_the_other_tab() {
        let manager = PreviewManager::new();
        let id = thread("one");
        manager.open(id.clone(), "a".into(), None, PreviewViewportSetting::Fill, PreviewAppearance::System, PreviewZoom::X100, None).unwrap();
        manager.open(id.clone(), "b".into(), None, PreviewViewportSetting::Fill, PreviewAppearance::System, PreviewZoom::X100, None).unwrap();
        assert_eq!(manager.close(&id, Some("a")), vec!["a"]);
        assert_eq!(manager.list(&id).sessions.iter().map(|s| s.tab_id.as_str()).collect::<Vec<_>>(), vec!["b"]);
    }

    #[test]
    fn reported_navigation_failure_updates_metadata_and_emits_failure() {
        let manager = PreviewManager::new();
        let id = thread("one");
        manager
            .open(
                id.clone(),
                "tab".into(),
                Some("http://localhost:5173"),
                PreviewViewportSetting::Fill,
                PreviewAppearance::System,
                PreviewZoom::X100,
                None,
            )
            .unwrap();
        let mut events = manager.subscribe();
        manager
            .report_status(
                &id,
                "tab",
                PreviewNavStatus::LoadFailed {
                    url: "http://localhost:5173/".into(),
                    title: "Preview".into(),
                    code: -2,
                    description: "offline".into(),
                },
                false,
                false,
            )
            .unwrap();
        let event = events.try_recv().unwrap();
        assert!(matches!(event, PreviewEvent::Failed { code: -2, .. }));
        assert!(matches!(
            manager.get(&id, "tab").unwrap().nav_status,
            PreviewNavStatus::LoadFailed { code: -2, .. }
        ));
    }

    #[test]
    fn repeated_frame_metadata_does_not_advance_the_revision() {
        let manager = PreviewManager::new();
        let id = thread("one");
        manager
            .open(
                id.clone(),
                "tab".into(),
                Some("http://localhost:5173"),
                PreviewViewportSetting::Fill,
                PreviewAppearance::System,
                PreviewZoom::X100,
                None,
            )
            .unwrap();
        let before = manager.list(&id).revision;
        let status = manager.get(&id, "tab").unwrap().nav_status;
        manager
            .report_status(&id, "tab", status, false, false)
            .unwrap();
        assert_eq!(manager.list(&id).revision, before);
    }

    #[test]
    fn recording_statuses_are_thread_scoped_and_publish_revision_changes() {
        let manager = PreviewManager::new();
        let id = thread("one");
        let other = thread("two");
        manager
            .open(
                id.clone(),
                "tab".into(),
                Some("http://localhost:5173"),
                PreviewViewportSetting::Fill,
                PreviewAppearance::System,
                PreviewZoom::X100,
                None,
            )
            .unwrap();
        let before = manager.list(&id).revision;
        manager
            .recording_started(
                id.clone(),
                PreviewRecordingStatus {
                    tab_id: "tab".into(),
                    recording_id: "recording".into(),
                    recording: true,
                    started_at: Some("2026-01-01T00:00:00Z".into()),
                },
            )
            .unwrap();
        assert_eq!(manager.list(&id).recordings.len(), 1);
        assert!(manager.list(&other).recordings.is_empty());
        assert!(manager.list(&id).revision > before);
        manager.recording_finished(&id, "tab", "recording");
        assert!(manager.list(&id).recordings.is_empty());
    }

    #[test]
    fn artifact_eviction_is_published_and_cleared_by_a_new_recording() {
        let manager = PreviewManager::new();
        let id = thread("one");
        manager
            .open(
                id.clone(),
                "tab".into(),
                Some("http://localhost:5173"),
                PreviewViewportSetting::Fill,
                PreviewAppearance::System,
                PreviewZoom::X100,
                None,
            )
            .unwrap();
        manager.recording_artifact_removed(&id, "tab");
        assert_eq!(manager.list(&id).invalidated_recordings, vec!["tab"]);
        let mut events = manager.subscribe();
        manager.recording_started(
            id.clone(),
            PreviewRecordingStatus {
                tab_id: "tab".into(),
                recording_id: "recording".into(),
                recording: true,
                started_at: Some("2026-01-01T00:00:00Z".into()),
            },
        ).unwrap();
        assert!(manager.list(&id).invalidated_recordings.is_empty());
        assert!(matches!(events.try_recv().unwrap(), PreviewEvent::RecordingChanged { .. }));
    }

    #[test]
    fn finishing_after_close_does_not_resurrect_recording_metadata() {
        let manager = PreviewManager::new();
        let id = thread("one");
        manager
            .open(
                id.clone(),
                "tab".into(),
                Some("http://localhost:5173"),
                PreviewViewportSetting::Fill,
                PreviewAppearance::System,
                PreviewZoom::X100,
                None,
            )
            .unwrap();
        manager
            .recording_started(
                id.clone(),
                PreviewRecordingStatus {
                    tab_id: "tab".into(),
                    recording_id: "recording".into(),
                    recording: true,
                    started_at: Some("2026-01-01T00:00:00Z".into()),
                },
            )
            .unwrap();
        manager.close(&id, Some("tab"));
        manager.recording_finished(&id, "tab", "recording");
        assert!(manager.list(&id).sessions.is_empty());
        assert!(manager.list(&id).recordings.is_empty());
    }

    #[test]
    fn late_monitor_completion_cannot_finish_a_reused_tab() {
        let manager = PreviewManager::new();
        let id = thread("one");
        manager
            .open(
                id.clone(),
                "tab".into(),
                Some("http://localhost:5173"),
                PreviewViewportSetting::Fill,
                PreviewAppearance::System,
                PreviewZoom::X100,
                None,
            )
            .unwrap();
        manager
            .recording_started(
                id.clone(),
                PreviewRecordingStatus {
                    tab_id: "tab".into(),
                    recording_id: "old".into(),
                    recording: true,
                    started_at: Some("old".into()),
                },
            )
            .unwrap();
        manager.recording_finished(&id, "tab", "other");
        assert_eq!(manager.list(&id).recordings[0].recording_id, "old");

        manager
            .recording_started(
                id.clone(),
                PreviewRecordingStatus {
                    tab_id: "tab".into(),
                    recording_id: "new".into(),
                    recording: true,
                    started_at: Some("new".into()),
                },
            )
            .unwrap();
        manager.recording_finished(&id, "tab", "old");
        assert_eq!(manager.list(&id).recordings[0].recording_id, "new");
    }
}
