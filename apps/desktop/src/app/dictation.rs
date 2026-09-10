use super::*;
use agent_core::state::operations as op;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Phase {
    Permission,
    Recording,
    Transcribing,
}
pub(super) struct Dictation {
    id: uuid::Uuid,
    key: String,
    generation: u64,
    pub(super) phase: Phase,
    send: bool,
    control: Option<platform::Recording>,
}
impl Desktop {
    pub(super) fn start_dictation(&mut self) {
        if self.dictation.is_some() || !self.snapshot.connected || self.busy > 0 {
            return;
        }
        let (events, incoming) = async_channel::unbounded();
        match platform::start_recording(events) {
            Ok(control) => {
                let id = uuid::Uuid::new_v4();
                let updates = self.updates.clone();
                self.runtime.handle.spawn(async move {
                    while let Ok(event) = incoming.recv().await {
                        if updates.send(Update::Recording(id, event)).await.is_err() {
                            break;
                        }
                    }
                });
                self.dictation = Some(Dictation {
                    id,
                    key: self.draft_key().into(),
                    generation: self.snapshot.epoch,
                    phase: Phase::Permission,
                    send: false,
                    control: Some(control),
                });
                self.error.clear();
            }
            Err(error) => self.error = error,
        }
    }
    pub(super) fn finish_dictation(&mut self, send: bool, _: &Context<Self>) {
        let Some(state) = self
            .dictation
            .as_mut()
            .filter(|state| state.phase == Phase::Recording)
        else {
            return;
        };
        if let Err(error) = state.control.as_mut().expect("recording control").finish() {
            self.error = error;
            self.dictation = None;
            return;
        }
        state.phase = Phase::Transcribing;
        state.send = send;
    }
    pub(super) fn cancel_recording(&mut self) {
        if let Some(state) = self.dictation.as_mut() {
            state.send = false;
            if state.phase != Phase::Transcribing {
                self.dictation = None;
            }
        }
    }
    pub(super) fn recording_update(
        &mut self,
        id: uuid::Uuid,
        event: platform::RecordingEvent,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        let Some(state) = self.dictation.as_mut().filter(|state| state.id == id) else {
            return;
        };
        match event {
            platform::RecordingEvent::Started => state.phase = Phase::Recording,
            platform::RecordingEvent::Finished(Err(error)) => {
                self.dictation = None;
                self.error = error;
            }
            platform::RecordingEvent::Finished(Ok(audio)) => {
                state.control = None;
                let intent = Intent::Transcribe(op::Transcribe {
                    draft_key: state.key.clone(),
                    audio: base64::engine::general_purpose::STANDARD.encode(audio),
                    send: state.send && state.generation == self.snapshot.epoch,
                    client_user_message_id: uuid::Uuid::new_v4().to_string(),
                });
                self.perform(intent, move |view, result, window, cx| {
                    if view.dictation.as_ref().is_some_and(|state| state.id == id) {
                        view.dictation = None;
                    }
                    if let Err(error) = result {
                        view.error = error;
                    }
                    view.accept_snapshot(window, cx);
                });
            }
        }
    }
}
