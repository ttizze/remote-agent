//! Capture is the platform's (AVFAudio, WASAPI); preparation, transcription
//! and draft updates are core's.
use crate::l10n::tr;
#[cfg(desktop)]
use crate::ui::colors;
use crate::ui::{self, Enabled, Icon};
use crate::{Connection, native};
use agent_core::{client::DictationPreparation, state::DraftKey};
use nsh_core::element::{rgb, rgba};
use nsh_core::prelude::*;
use std::sync::Arc;

const LEVEL_COUNT: usize = 40;

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod avfaudio;
#[cfg(any(target_os = "macos", target_os = "ios"))]
use avfaudio::Capture;
#[cfg(windows)]
mod wasapi;
#[cfg(windows)]
use wasapi::Capture;

struct Recording {
    capture: Capture,
    preparation: Arc<DictationPreparation>,
    host: String,
    pane: Option<u64>,
    draft: DraftKey,
}

#[derive(Clone, Copy)]
pub(crate) struct Dictation {
    owner: Owner,
    recording: Stored<Option<Recording>>,
    pub active: Signal<bool>,
    pub requesting: Signal<bool>,
    levels: Signal<[f32; LEVEL_COUNT]>,
    send_recorded_text: Signal<bool>,
    generation: Signal<u64>,
    target: Memo<(String, DraftKey)>,
}

impl Dictation {
    pub fn new() -> Self {
        let connection = use_context::<Connection>().unwrap();
        let this = Self {
            owner: Owner::current(),
            recording: stored(None),
            active: signal(false),
            requesting: signal(false),
            levels: signal([0.0; LEVEL_COUNT]),
            send_recorded_text: signal(false),
            generation: signal(0),
            target: computed(move || {
                (
                    connection.host.get(),
                    connection
                        .composer
                        .with(|p| p.controls.navigation.draft_key.clone()),
                )
            }),
        };
        effect(move || {
            this.target.get();
            this.cancel();
        });
        // The iPhone stops recording when it leaves the foreground; the Mac goes
        // on while the user is in another app, as its desktop client did.
        effect(move || {
            if !native::in_use() || !connection.composer.with(|p| p.controls.connected) {
                this.cancel();
            }
        });
        effect(move || {
            if this.active.get() {
                // Only the bars subscribe to levels. The timer ends with this
                // recording's effect, without redrawing the rest of the chat.
                set_interval(std::time::Duration::from_millis(50), move || {
                    if let Some(level) = this
                        .recording
                        .with(|recording| recording.as_ref().map(|r| r.capture.level()))
                    {
                        this.levels.update(|levels| {
                            levels.rotate_left(1);
                            levels[LEVEL_COUNT - 1] = if level.is_finite() {
                                level.clamp(0.0, 1.0)
                            } else {
                                0.0
                            };
                        });
                    }
                });
            }
        });
        this
    }

    pub fn cancel(self) {
        self.generation.update(|value| *value += 1);
        self.recording.replace(None);
        self.active.set(false);
        self.requesting.set(false);
    }

    pub fn finish(self, client: Arc<crate::client::Client>, send: bool) {
        if let Some(recording) = self.recording.replace(None) {
            self.active.set(false);
            self.send_recorded_text.set(send);
            match recording.capture.finish() {
                Ok(audio) => client.dictate(
                    recording.host,
                    recording.pane,
                    recording.draft,
                    recording.preparation,
                    audio,
                    send,
                ),
                Err(reason) => native::message(tr("音声入力"), reason),
            }
        }
    }

    pub fn body(self) -> View {
        let width = signal(0.0_f32);
        view()
            .h(px(48.0))
            .min_w_0()
            .justify_center()
            .child(when(
                move || self.active.get(),
                move || {
                    (0..LEVEL_COUNT).fold(
                        view()
                            .flex_row()
                            .w_full()
                            .h(px(48.0))
                            .items_center()
                            .justify_center()
                            .gap(move || {
                                px(if is_desktop() {
                                    3.0
                                } else {
                                    (width.get() / LEVEL_COUNT as f32 - 3.0).max(0.0)
                                })
                            })
                            .overflow_hidden()
                            .label(tr("録音中"))
                            .on_layout(move |bounds: &Bounds<Points>| {
                                width.set(bounds.size.width.0)
                            }),
                        |waveform, index| {
                            waveform.child(
                                view()
                                    .w(px(3.0))
                                    .flex_shrink_0()
                                    .h(move || {
                                        px(3.0
                                            + self.levels.with(|levels| levels[index].sqrt())
                                                * 45.0)
                                    })
                                    .rounded_full()
                                    .bg(move || nsh_base::theme().foreground),
                            )
                        },
                    )
                },
            ))
            .child(when(
                move || !self.active.get(),
                move || {
                    view()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(ui::spinner())
                        .child(text(move || {
                            if self.requesting.get() {
                                tr("マイクの許可を確認中…")
                            } else if !is_desktop() && self.send_recorded_text.get() {
                                tr("文字起こしして送信中…")
                            } else {
                                tr("文字起こし中…")
                            }
                        }))
                },
            ))
    }

    pub fn cancel_button(self) -> impl Element {
        let connection = use_context::<Connection>().unwrap();
        ui::icon_button_sized(
            Icon::CLOSE,
            tr("録音を取り消す"),
            40.0,
            20.0,
            move || self.cancel(),
        )
        .enabled(move || !connection.composer.with(|p| p.controls.transcribing))
    }

    #[cfg(desktop)]
    pub fn bar(
        self,
        send: impl Fn() + 'static,
        enabled: impl Fn() -> bool + Copy + 'static,
    ) -> View {
        let connection = use_context::<Connection>().unwrap();
        view()
            .flex_row()
            .h(px(88.0))
            .px_2()
            .items_center()
            .gap_3()
            .child(self.cancel_button())
            .child(self.body().flex_1())
            .child(
                ui::icon_button_sized(
                    Icon::STOP,
                    tr("録音を終了して文字起こし"),
                    40.0,
                    20.0,
                    move || self.finish(connection.client.get(), false),
                )
                .enabled(move || self.active.get()),
            )
            .child(
                ui::ghost(
                    tr("文字起こしして送信"),
                    view().child(ui::icon(Icon::ARROW_UP, 24.0)),
                )
                .w(px(44.0))
                .h(px(44.0))
                .rounded_full()
                .bg(colors::foreground())
                .text_color(colors::SIDEBAR)
                .on_press(send)
                .enabled(move || self.active.get() && enabled()),
            )
    }

    pub fn button(self) -> impl Element {
        let connection = use_context::<Connection>().unwrap();
        ui::ghost(
            move || -> SharedString {
                tr(if self.active.get() {
                    "録音を終了して文字起こし"
                } else {
                    "音声をCodexで文字起こし"
                })
                .into()
            },
            view()
                .child(when(move || self.requesting.get(), ui::spinner))
                .child(when(
                    move || !self.requesting.get(),
                    move || {
                        view()
                            .w(px(34.0))
                            .h(px(34.0))
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .style_when(
                                move || self.active.get(),
                                |s| s.bg(rgba(0xff3b301f)).text_color(rgb(0xff3b30)),
                            )
                            .child(dynamic(move || {
                                ui::icon(
                                    if self.active.get() {
                                        Icon::STOP
                                    } else {
                                        Icon::MICROPHONE
                                    },
                                    20.0,
                                )
                            }))
                    },
                )),
        )
        .w(px(44.0))
        .h(px(44.0))
        .enabled(move || {
            !self.requesting.get()
                && !connection.uploading()
                && connection.composer.with(|p| p.can_dictate())
        })
        .on_press(move || {
            if self.active.get_untracked() {
                self.finish(connection.client.get(), false);
            } else {
                self.start();
            }
        })
    }

    fn start(self) {
        let connection = use_context::<Connection>().unwrap();
        let (host, draft) = self.target.get_untracked();
        let preparation = match connection
            .client
            .with(|client| client.prepare_dictation(&host, connection.pane))
        {
            Ok(preparation) => preparation,
            Err(reason) => {
                native::message(tr("音声入力"), reason.to_string());
                return;
            }
        };
        self.requesting.set(true);
        let current = self.generation.get_untracked();
        // The permission request outlives the button replaced by the recording bar.
        self.owner.run(move || {
            spawn(async move {
                let permission = Capture::request_permission().await;
                if current != self.generation.get_untracked()
                    || self.target.get_untracked() != (host.clone(), draft.clone())
                {
                    return;
                }
                self.requesting.set(false);
                let result = permission.and_then(|()| Capture::start());
                match result {
                    Ok(capture) => {
                        self.recording.replace(Some(Recording {
                            capture,
                            preparation,
                            host,
                            pane: connection.pane,
                            draft,
                        }));
                        self.levels.set([0.0; LEVEL_COUNT]);
                        self.active.set(true);
                    }
                    Err(reason) => native::message(tr("音声入力"), reason),
                }
            })
        });
    }
}
