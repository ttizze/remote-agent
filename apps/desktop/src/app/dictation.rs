//! Recording a spoken prompt and sending it to the Host for transcription.
use super::{Desktop, Update};
use crate::platform;
use agent_core::state::Intent;
use gpui_kit::*;

pub(crate) struct Dictation {
    pub(crate) id: uuid::Uuid,
    key: String,
    pub(crate) label: &'static str,
    pub(crate) recording: bool,
    control: Option<platform::Recording>,
    preparation: Option<agent_core::client::DictationPreparation>,
    pub(crate) level: f32,
}

impl Desktop {
    /// Starts recording into the draft the composer shows.
    pub(crate) fn start_dictation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dictation.is_some() || !self.snapshot.connected {
            return;
        }
        let (tx, rx) = async_channel::bounded(64);
        match platform::start_recording(tx) {
            Err(error) => self.show_error(&error, window, cx),
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
        cx.notify();
    }

    /// Stops recording and transcribes what was said.
    pub(crate) fn finish_dictation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(state) = &mut self.dictation
            && let Some(control) = &state.control
        {
            if let Err(error) = control.finish() {
                self.dictation = None;
                self.show_error(&error, window, cx);
                return;
            }
            state.recording = false;
            state.label = "Transcribing…";
        }
        cx.notify();
    }

    pub(crate) fn cancel_dictation(&mut self, cx: &mut Context<Self>) {
        self.dictation = None;
        cx.notify();
    }

    pub(super) fn recording_update(
        &mut self,
        id: uuid::Uuid,
        event: platform::RecordingEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = self.store();
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
                state.preparation = store.as_ref().map(|store| store.prepare_dictation());
            }
            platform::RecordingEvent::Level(level) => {
                if level.is_finite() {
                    state.level = level.clamp(0., 1.);
                }
            }
            platform::RecordingEvent::Finished(Err(error)) => {
                self.dictation = None;
                self.show_error(&error, window, cx);
            }
            platform::RecordingEvent::Finished(Ok(audio)) => {
                let intent = Intent::Transcribe {
                    draft_key: state.key.clone(),
                    preparation: state.preparation.as_ref().map(|p| p.id()),
                    audio,
                };
                if let Some(store) = store {
                    let updates = self.updates.clone();
                    let epoch = self.epoch;
                    self.runtime.handle.spawn(async move {
                        let result = store
                            .dispatch(intent)
                            .await
                            .map_err(|e| e.to_string())
                            .and_then(|result| result.map_err(|e| e.to_string()));
                        let _ = updates.send((epoch, Update::Transcribed(id, result))).await;
                    });
                }
                state.control = None;
                state.label = "Transcribing…";
            }
        }
        cx.notify();
    }
}
