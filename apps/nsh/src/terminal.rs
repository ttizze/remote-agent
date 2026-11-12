use crate::l10n::{localized, tr};
use crate::ui::{self, Enabled, Icon, colors};
use crate::{Connection, shared::Shared, workbench::Workbench};
use agent_core::{
    panels::PanelSurface,
    state::{Intent, operations as op},
};
use nsh_core::prelude::*;

pub(crate) fn screen(surface: PanelSurface, host: String) -> impl IntoElement {
    let connection = use_context::<Connection>().unwrap();
    let workspace = use_context::<Workbench>().unwrap();
    let host = stored(host);
    let cwd = stored(surface.path.unwrap_or_default());
    let command = stored(surface.command);
    #[cfg(target_os = "macos")]
    let id = surface.id;
    let handle = stored(workspace.terminal_handle(surface.id));
    // The view's owner releases its stored values before its cleanup runs, so
    // what the cleanup reads lives outside them.
    let started = std::rc::Rc::new(std::cell::Cell::new(false));
    // A grid that goes away with the keyboard gives it up.
    let focus = use_context::<Focus>().unwrap();
    let focused = std::rc::Rc::new(std::cell::Cell::new(false));
    cleanup({
        let focused = focused.clone();
        move || {
            if focused.get() {
                focus.set(false);
            }
        }
    });
    let size = signal(None::<agent_protocol::operations::TerminalSize>);
    let resource = computed(move || {
        connection
            .publication
            .with(|p| p.snapshot.terminals.get(&handle.get()).cloned().map(Shared))
    });
    let terminal = computed(move || {
        resource.get();
        connection.publication.with_untracked(|p| {
            p.snapshot
                .terminals()
                .view(crate::l10n::language(), handle.get())
        })
    });
    let input = computed(move || {
        terminal.with(|terminal| {
            terminal
                .as_ref()
                .is_some_and(|terminal| terminal.accepts_input)
        })
    });
    let loading = computed(move || {
        terminal.with(|terminal| terminal.as_ref().is_some_and(|terminal| terminal.loading))
    });
    let send = move |intent| {
        connection
            .client
            .with(|client| client.dispatch(&host.get(), None, intent))
    };
    let start = move |started: &std::cell::Cell<bool>, size| {
        started.set(true);
        send(Intent::StartTerminal(op::StartTerminal {
            command: command.get(),
            handle: handle.get(),
            cwd: cwd.get(),
            size,
        }));
    };
    let resize_target = computed(move || (size.get(), input.get()));
    effect(move || {
        let (Some(size), true) = resize_target.get() else {
            return;
        };
        let actual =
            resource.with_untracked(|terminal| terminal.as_ref().map(|terminal| terminal.size));
        if actual != Some(size) {
            // Layout can finish before the Starting publication arrives. Retain that
            // size and apply it once core accepts input, including after reconnecting.
            send(Intent::ResizeTerminal(op::ResizeTerminal {
                handle: handle.get(),
                size,
            }));
        }
    });
    cleanup({
        let started = started.clone();
        let (host, handle) = (host.get(), handle.get());
        move || {
            if started.get() {
                connection.client.with(|client| {
                    client.dispatch(
                        &host,
                        None,
                        Intent::DetachTerminal(op::DetachTerminal { handle }),
                    )
                });
            }
        }
    });
    // T3 Code's terminal fills its panel; Bex's folder, status and reconnection
    // sit in one quiet row above it, with stopping what runs in it.
    view()
        .flex_1()
        .min_h_0()
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_2()
                .h(px(32.0))
                .flex_shrink_0()
                .px_3()
                .border_b_1()
                .border_b_color(colors::border())
                .text_color(colors::muted())
                .child(ui::icon(Icon::TERMINAL, 14.0))
                .child(
                    text(move || cwd.get())
                        .text_size(px(12.0))
                        .selectable(true)
                        .truncate()
                        .flex_1()
                        .min_w_0(),
                )
                .child(when(move || loading.get(), ui::spinner))
                .child(
                    text(move || {
                        if loading.get() {
                            tr("接続中…").to_owned()
                        } else {
                            terminal.with(|terminal| {
                                terminal
                                    .as_ref()
                                    .and_then(|terminal| terminal.status.clone())
                                    .map(localized)
                                    .unwrap_or_default()
                            })
                        }
                    })
                    .text_size(px(12.0))
                    .number_of_lines(1),
                )
                .child(when(
                    move || {
                        terminal.with(|terminal| {
                            terminal
                                .as_ref()
                                .is_some_and(|terminal| terminal.status.is_some())
                        })
                    },
                    {
                        let started = started.clone();
                        move || {
                            let started = started.clone();
                            ui::text_button(tr("再接続"), move || {
                                if let Some(size) = size.get_untracked() {
                                    start(&started, size);
                                }
                            })
                            .enabled(move || {
                                size.get().is_some()
                                    && connection.settings.with(|p| p.connected())
                                    && !loading.get()
                            })
                        }
                    },
                ))
                .child(
                    ui::text_button(tr("終了"), move || {
                        send(Intent::KillTerminal(op::KillTerminal {
                            handle: handle.get(),
                        }));
                    })
                    .enabled(move || input.get()),
                ),
        )
        .child(grid(
            move || input.get(),
            move || {
                terminal.with(|terminal| {
                    terminal
                        .as_ref()
                        .map(|terminal| terminal.output.clone())
                        .unwrap_or_default()
                })
            },
            {
                let started = started.clone();
                move |report| match report {
                    Report::Resize(next) => {
                        if size.get_untracked() == Some(next) {
                            return;
                        }
                        size.set(Some(next));
                        if !started.get() {
                            start(&started, next);
                        }
                    }
                    Report::Input(data) if input.get_untracked() => {
                        send(Intent::WriteTerminal(op::WriteTerminal {
                            handle: handle.get(),
                            data,
                        }));
                    }
                    Report::Input(_) => {}
                    Report::Ack(sequence) => send(Intent::AcknowledgeTerminal {
                        handle: handle.get(),
                        sequence,
                    }),
                    Report::Focus(now) => {
                        if focused.replace(now) != now {
                            focus.set(now);
                        }
                    }
                    #[cfg(target_os = "macos")]
                    Report::Quote(text) => {
                        let Some(scope) = connection
                            .state
                            .with_untracked(|state| state.panels.scope(id))
                        else {
                            return;
                        };
                        let visible = workspace.scope.get_untracked() == scope;
                        connection.client.with(|client| {
                            client.dispatch(
                                &scope.host,
                                None,
                                Intent::QuoteSelection {
                                    storage_scope: scope.storage,
                                    thread_id: scope.draft,
                                    text,
                                },
                            )
                        });
                        if visible {
                            after_answer(move || connection.composer_ref.focus());
                        }
                    }
                }
            },
        ))
}

/// What the grid reports to the terminal it shows.
pub(crate) enum Report {
    Resize(agent_protocol::operations::TerminalSize),
    Input(Vec<u8>),
    Ack(u64),
    /// The grid took or gave up the keyboard.
    Focus(bool),
    #[cfg(target_os = "macos")]
    Quote(String),
}

/// Whether a terminal holds the keyboard, for shortcuts that T3 Code binds
/// everywhere but the terminal (`!terminalFocus`).
#[derive(Clone, Copy)]
pub(crate) struct Focus(Signal<u32>);

impl Focus {
    pub(crate) fn new() -> Self {
        Self(signal(0))
    }
    /// Counted, so one terminal's blur cannot clear another's focus.
    fn set(self, focused: bool) {
        self.0.update(|count| {
            *count = if focused {
                *count + 1
            } else {
                count.saturating_sub(1)
            }
        });
    }
    pub(crate) fn get(self) -> bool {
        self.0.get() > 0
    }
}

/// The terminal's grid: SwiftTerm on Apple platforms, which reports through
/// its native events.
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn grid(
    input: impl Fn() -> bool + 'static,
    output: impl Fn() -> Vec<agent_core::state::TerminalOutput> + 'static,
    report: impl Fn(Report) + 'static,
) -> impl IntoElement {
    use agent_core::presentation::terminals::terminal_grid;
    use serde_json::json;
    let grid = native("bex.terminal")
        .prop("input", move || json!(input()))
        .prop("output", move || json!(output()));
    #[cfg(target_os = "macos")]
    let grid = grid.prop("menu", move || {
        json!({
            "quote": tr("チャットに追加"),
            "copy": tr("コピー"),
            "paste": tr("貼り付け"),
            "selectAll": tr("すべて選択"),
            "bottom": tr("最新の出力に移動"),
        })
    });
    grid.on_native_event(move |event: &NativeEvent| {
        let data = &event.data;
        match &*event.name {
            "resize" => {
                if let Some(size) = data["cols"]
                    .as_i64()
                    .and_then(|cols| i32::try_from(cols).ok())
                    .zip(
                        data["rows"]
                            .as_i64()
                            .and_then(|rows| i32::try_from(rows).ok()),
                    )
                    .and_then(|(cols, rows)| terminal_grid(cols, rows))
                {
                    report(Report::Resize(size));
                }
            }
            "input" => {
                if let Ok(bytes) = serde_json::from_value::<Vec<u8>>(data.clone()) {
                    report(Report::Input(bytes));
                }
            }
            "ack" => {
                if let Some(sequence) = data.as_u64() {
                    report(Report::Ack(sequence));
                }
            }
            "focus" => {
                if let Some(focused) = data.as_bool() {
                    report(Report::Focus(focused));
                }
            }
            #[cfg(target_os = "macos")]
            "quote" => {
                if let Some(text) = data.as_str().filter(|text| !text.is_empty()) {
                    report(Report::Quote(text.to_owned()));
                }
            }
            _ => {}
        }
    })
    .flex_1()
    .min_h_0()
}

/// The terminal's grid elsewhere: the screen NSH draws from the same bytes.
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
use crate::terminal_screen::grid;
