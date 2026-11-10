//! Plan exports belong to the conversation, so folding or recycling a plan's
//! row cannot cancel a save or release the file while an OS picker uses it.
use crate::{
    Connection,
    l10n::tr,
    native,
    ui::{self, Enabled, colors},
};
use agent_core::{
    presentation::conversation::{ActionTone, plan_export},
    state::{Intent, operations as op},
};
use nsh_core::prelude::*;

#[derive(Clone, PartialEq)]
struct WorkspacePlan {
    host: String,
    cwd: String,
    markdown: String,
}

#[derive(Clone, Copy)]
pub(crate) struct Saver {
    plan: Signal<Option<WorkspacePlan>>,
    busy: Signal<bool>,
    failure: Signal<String>,
    owner: Owner,
    #[cfg(target_os = "ios")]
    exported: Signal<Option<crate::shared::Shared<crate::client::LocalFile>>>,
}

impl Saver {
    pub fn new() -> Self {
        Self {
            plan: signal(None),
            busy: signal(false),
            failure: signal(String::new()),
            owner: Owner::current(),
            #[cfg(target_os = "ios")]
            exported: signal(None),
        }
    }

    fn open(self, host: String, cwd: String, markdown: String) {
        if self.busy.get_untracked() {
            return;
        }
        self.failure.set(String::new());
        self.plan.set(Some(WorkspacePlan {
            host,
            cwd,
            markdown,
        }));
    }

    fn cancel(self) {
        if self.busy.try_get() == Some(false) {
            self.plan.set(None);
        }
    }

    fn save(self, connection: Connection, path: String) {
        let Some(plan) = self.plan.try_get().flatten() else {
            return;
        };
        if self.busy.get_untracked() || path.trim().is_empty() {
            return;
        }
        self.busy.set(true);
        self.failure.set(String::new());
        let response = connection.client.with(|client| {
            client.request(
                &plan.host,
                connection.pane,
                Intent::WriteWorkspaceFile(op::WriteWorkspaceFile {
                    cwd: plan.cwd,
                    relative_path: path,
                    text: plan.markdown,
                }),
            )
        });
        self.owner.run(move || {
            spawn(async move {
                let result = response.recv().await;
                self.busy.set(false);
                match result {
                    Ok(Ok(_)) => self.plan.set(None),
                    Ok(Err(reason)) => self.failure.set(reason.to_string()),
                    Err(reason) => self.failure.set(reason.to_string()),
                }
            })
        });
    }

    pub fn view(self, connection: Connection) -> View {
        let root = view().child(self.editor(connection));
        #[cfg(target_os = "ios")]
        let root = root.child(
            nsh_core::element::native("bex.filePicker")
                .prop("export", move || {
                    serde_json::json!(self.exported.with(|file| {
                        file.as_ref()
                            .map(|file| file.path.to_string_lossy().into_owned())
                    }))
                })
                .on_native_event(move |_: &NativeEvent| self.exported.set(None))
                .w(px(0.0))
                .h(px(0.0)),
        );
        root
    }

    fn editor(self, connection: Connection) -> AnyElement {
        when(
            move || self.plan.with(Option::is_some),
            move || {
                let plan = stored(self.plan.get_untracked().unwrap());
                let path = signal(plan_export(&plan.get().markdown).0);
                nsh_base::dialog()
                    .presentation(ModalPresentation::FormSheet)
                    .close_on_swipe(move || !self.busy.get())
                    .close_on_escape(move || !self.busy.get())
                    .close_on_backdrop_press(move || !self.busy.get())
                    .on_cancel(move || self.cancel())
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .child(
                        view()
                            .p_5()
                            .gap_3()
                            .w_full()
                            .max_w(px(480.0))
                            .bg(colors::SURFACE)
                            .rounded(px(12.0))
                            .child(
                                text(tr("ワークスペースに保存…"))
                                    .text_size(px(18.0))
                                    .font_weight(FontWeight::SEMIBOLD),
                            )
                            .child(
                                text(move || plan.get().cwd)
                                    .text_size(px(12.0))
                                    .text_color(colors::MUTED)
                                    .selectable(true),
                            )
                            .child(text(tr("ワークスペース内のパス")).text_size(px(13.0)))
                            .child(
                                ui::field()
                                    .value(path)
                                    .on_change(move |value: &str| path.set(value.into()))
                                    .on_submit(move |_: &str| {
                                        self.save(connection, path.get_untracked())
                                    })
                                    .disabled(self.busy)
                                    .label(tr("ワークスペース内のパス")),
                            )
                            .child(when(
                                move || !self.failure.get().is_empty(),
                                move || {
                                    text(move || self.failure.get())
                                        .text_color(crate::conversation::look::DESTRUCTIVE)
                                        .selectable(true)
                                },
                            ))
                            .child(
                                view()
                                    .flex_row()
                                    .justify_end()
                                    .gap_2()
                                    .child(
                                        ui::text_button(tr("キャンセル"), move || {
                                            self.cancel()
                                        })
                                        .enabled(move || !self.busy.get()),
                                    )
                                    .child(
                                        ui::action_button(
                                            tr("保存"),
                                            ActionTone::Primary,
                                            move || self.save(connection, path.get_untracked()),
                                        )
                                        .enabled(
                                            move || {
                                                !self.busy.get() && !path.get().trim().is_empty()
                                            },
                                        ),
                                    ),
                            ),
                    )
            },
        )
        .into_element()
    }

    fn download(self, filename: String, text: String) {
        self.owner.run(move || {
            #[cfg(not(target_os = "ios"))]
            download(filename, text);
            #[cfg(target_os = "ios")]
            spawn(async move {
                let result = blocking(move || crate::client::LocalFile::markdown(filename, text))
                    .await
                    .map_err(|reason| reason.to_string())
                    .and_then(|result| result.map_err(|reason| reason.to_string()));
                match result {
                    Ok(file) => self
                        .exported
                        .set(Some(crate::shared::Shared(std::sync::Arc::new(file)))),
                    Err(reason) => native::message(tr("Markdownとして保存…"), reason),
                }
            });
        });
    }
}

pub(crate) fn actions(
    saver: Saver,
    connection: Connection,
    markdown: impl Fn() -> Option<String> + Copy + 'static,
) -> NativeView {
    native::menu(
        "ellipsis",
        tr("計画の操作"),
        move || {
            let enabled = saver.busy.try_get() == Some(false)
                && connection
                    .composer
                    .try_get()
                    .is_some_and(|p| p.controls.connected && !p.controls.navigation.cwd.is_empty());
            serde_json::json!([
                {"id":"copy", "title":tr("クリップボードにコピー")},
                {"id":"download", "title":tr("Markdownとして保存…")},
                {"id":"workspace", "title":tr("ワークスペースに保存…"), "enabled":enabled}
            ])
        },
        move |id| {
            let Some(markdown) = markdown() else {
                return;
            };
            let (filename, text) = plan_export(&markdown);
            match id {
                "copy" => set_clipboard(text),
                "download" => saver.download(filename, text),
                "workspace" => saver.open(
                    connection.host.get_untracked(),
                    connection
                        .composer
                        .with_untracked(|p| p.controls.navigation.cwd.clone()),
                    text,
                ),
                _ => {}
            }
        },
    )
    .w(px(24.0))
    .h(px(24.0))
}

#[cfg(not(target_os = "ios"))]
fn download(filename: String, text: String) {
    let chosen = pick_save_path(
        nsh_core::file_dialog::SaveDialog::new()
            .file_name(filename)
            .filter(nsh_core::file_dialog::FileFilter::new("Markdown", ["md"])),
    );
    spawn(async move {
        let result = match chosen.await {
            Ok(Some(path)) => blocking(move || std::fs::write(path, text))
                .await
                .map_err(|reason| reason.to_string())
                .and_then(|result| result.map_err(|reason| reason.to_string())),
            Ok(None) => return,
            Err(reason) => Err(reason.to_string()),
        };
        if let Err(reason) = result {
            native::message(tr("Markdownとして保存…"), reason);
        }
    });
}
