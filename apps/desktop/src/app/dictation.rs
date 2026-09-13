use super::*;
use agent_core::state::operations as op;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Phase {
    Permission,
    Recording,
    Transcribing,
}
pub(super) struct Dictation {
    pub(super) id: uuid::Uuid,
    key: String,
    generation: u64,
    pub(super) phase: Phase,
    send: bool,
    control: Option<platform::Recording>,
    pub(super) levels: std::collections::VecDeque<f32>,
}
impl Desktop {
    pub(super) fn dictation_bar(&self, cx: &Context<Self>) -> AnyElement {
        let Some(state) = &self.dictation else {
            return div().into_any_element();
        };
        let recording = state.phase == Phase::Recording;
        h_flex()
            .h(px(88.))
            .px_2()
            .gap_3()
            .child(
                self.icon_button(
                    "cancel-dictation",
                    IconName::Close,
                    "録音を取り消す (Esc)",
                    cx,
                    |s, _, _| s.cancel_recording(),
                )
                .disabled(state.phase == Phase::Transcribing),
            )
            .child(if recording {
                h_flex()
                    .id("recording-waveform")
                    .flex_1()
                    .min_w_0()
                    .h(px(48.))
                    .items_center()
                    .justify_center()
                    .gap(px(3.))
                    .overflow_hidden()
                    .children(state.levels.iter().map(|level| {
                        div()
                            .w(px(3.))
                            .flex_shrink_0()
                            .h(px(3. + level.sqrt() * 45.))
                            .rounded_full()
                            .bg(rgb(0xececec))
                    }))
                    .into_any_element()
            } else {
                h_flex()
                    .flex_1()
                    .gap_2()
                    .child(spinner::Spinner::new().small())
                    .child(if state.phase == Phase::Permission {
                        "マイクの許可を確認中…"
                    } else {
                        "文字起こし中…"
                    })
                    .into_any_element()
            })
            .child(
                self.icon_button(
                    "stop-dictation",
                    IconName::Pause,
                    "録音を終了して文字起こし",
                    cx,
                    |s, _, cx| s.finish_dictation(false, cx),
                )
                .icon(Icon::default().path("bex/stop.svg"))
                .disabled(!recording),
            )
            .child(
                self.icon_button(
                    "send-dictation",
                    IconName::ArrowUp,
                    "文字起こしして送信",
                    cx,
                    |s, _, cx| s.send(cx),
                )
                .primary()
                .large()
                .rounded_full()
                .w(px(44.))
                .h(px(44.))
                .disabled(!recording || !self.snapshot.connected),
            )
            .into_any_element()
    }
    pub(super) fn start_dictation(&mut self) {
        if self.dictation.is_some() || !self.snapshot.connected || self.busy > 0 {
            return;
        }
        let (events, incoming) = async_channel::unbounded();
        match platform::start_recording(events) {
            Ok(control) => {
                let id = uuid::Uuid::new_v4();
                let updates = self.updates.clone();
                let epoch = self.epoch;
                self.runtime.handle.spawn(async move {
                    while let Ok(event) = incoming.recv().await {
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
                    key: self.draft_key().into(),
                    generation: self.snapshot.epoch,
                    phase: Phase::Permission,
                    send: false,
                    control: Some(control),
                    levels: std::collections::VecDeque::from(vec![0.; 40]),
                });
                self.error.clear();
            }
            Err(error) => self.set_error(error),
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
            self.set_error(error);
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
            platform::RecordingEvent::Level(level) => {
                if state.phase == Phase::Recording && level.is_finite() {
                    state.levels.pop_front();
                    state.levels.push_back(level.clamp(0., 1.));
                }
            }
            platform::RecordingEvent::Finished(Err(error)) => {
                self.dictation = None;
                self.set_error(error);
            }
            platform::RecordingEvent::Finished(Ok(audio)) => {
                state.control = None;
                let intent = Intent::Transcribe(op::Dictate {
                    draft_key: state.key.clone(),
                    audio,
                    send: state.send && state.generation == self.snapshot.epoch,
                    client_user_message_id: uuid::Uuid::new_v4().to_string(),
                });
                self.perform(intent, OperationCompletion::Dictation(id));
            }
        }
    }
}
