use super::*;
pub(super) struct Dictation {
    id: uuid::Uuid,
    key: String,
    pub(super) label: &'static str,
    pub(super) recording: bool,
    control: Option<platform::Recording>,
    preparation: Option<agent_core::client::DictationPreparation>,
    pub(super) level: f32,
}
impl Desktop {
    pub(super) fn start_dictation(&mut self) {
        if self.dictation.is_some() || !self.snapshot.connected {
            return;
        }
        let (tx, rx) = async_channel::bounded(64);
        match platform::start_recording(tx) {
            Err(error) => self.error = error,
            Ok(control) => {
                let id = uuid::Uuid::new_v4();
                let updates = self.updates.clone();
                let epoch = self.epoch;
                self.runtime.handle.spawn(async move {
                    while let Ok(event) = rx.recv().await {
                        if updates
                            .send((epoch, Update::Recording(id, event)))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                });
                self.dictation = Some(Dictation {
                    id,
                    key: self.snapshot.draft_key(),
                    label: "Preparing microphone…",
                    recording: false,
                    control: Some(control),
                    preparation: None,
                    level: 0.,
                });
            }
        }
    }
    pub(super) fn finish_dictation(&mut self) {
        if let Some(state) = &mut self.dictation
            && let Some(control) = &state.control
        {
            if let Err(error) = control.finish() {
                self.error = error;
                self.dictation = None;
                return;
            }
            state.recording = false;
            state.label = "Transcribing…";
        }
    }
    pub(super) fn cancel_recording(&mut self) {
        self.dictation = None;
    }
    pub(super) fn recording_update(&mut self, id: uuid::Uuid, event: platform::RecordingEvent) {
        let Some(state) = self.dictation.as_mut().filter(|state| state.id == id) else {
            return;
        };
        match event {
            #[cfg(target_os = "macos")]
            platform::RecordingEvent::RequestingPermission => {
                state.label = "Allow microphone access…"
            }
            #[cfg(target_os = "macos")]
            platform::RecordingEvent::Preparing => state.label = "Preparing microphone…",
            platform::RecordingEvent::Started => {
                state.recording = true;
                state.label = "Recording…";
                state.preparation = self
                    .session
                    .as_ref()
                    .map(|session| session.store.prepare_dictation());
            }
            platform::RecordingEvent::Level(level) => {
                if level.is_finite() {
                    state.level = level.clamp(0., 1.);
                }
            }
            platform::RecordingEvent::Finished(Err(error)) => {
                self.error = error;
                self.dictation = None;
            }
            platform::RecordingEvent::Finished(Ok(audio)) => {
                let preparation = state.preparation.take();
                let intent = Intent::Transcribe {
                    draft_key: state.key.clone(),
                    preparation: preparation.as_ref().map(|p| p.id()),
                    audio,
                };
                if let Some(session) = &self.session {
                    let store = session.store.clone();
                    let updates = self.updates.clone();
                    let epoch = self.epoch;
                    self.runtime.handle.spawn(async move {
                        let result = store
                            .dispatch(intent)
                            .await
                            .map_err(|e| e.to_string())
                            .and_then(|result| result.map_err(|e| e.to_string()));
                        drop(preparation);
                        let _ = updates.send((epoch, Update::Completed(None, result))).await;
                    });
                }
                self.dictation = None;
            }
        }
    }
}
