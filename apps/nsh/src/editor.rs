use crate::l10n::{localized, tr};
use crate::{
    Connection,
    files::{Browser, TextFile, notice},
    media::Preview,
    ui::{self, Enabled, Icon, colors},
};
use agent_core::{
    presentation::files::{
        FileRendering, file_edit_prompt, file_line_start_utf16, file_rendered, file_rendering,
        mermaid_file_markdown,
    },
    state::{Intent, operations as op},
};
use nsh_core::{element::input, prelude::*};

mod table;

pub(crate) fn screen(target: ReadSignal<TextFile>) -> impl Element {
    let connection = use_context::<Connection>().unwrap();
    let browser = use_context::<Browser>().unwrap();
    let preview = use_context::<Preview>().unwrap();
    let path = stored(target.with_untracked(|target| target.path.clone()));
    let host = stored(connection.host.get_untracked());
    let draft = stored(
        connection
            .composer
            .with_untracked(|p| p.controls.navigation.draft_key.clone()),
    );
    let controls = computed(move || {
        connection.publication.with(|p| {
            p.pane()
                .file_editor(&p.snapshot.files.file_drafts, &path.get())
        })
    });
    let busy = move || {
        connection.publication.with(|p| {
            p.snapshot.operation_running(op::OperationKey::File {
                pane: p.pane_id.clone(),
            }) || p
                .snapshot
                .operation_running(op::OperationKey::SaveFile { path: path.get() })
        })
    };
    let field = element_ref();
    let composing = signal(false);
    let laid_out = signal(false);
    let surroundings = crate::rich_text::Surroundings::current();
    let offered = file_rendering(&path.get());
    let preferences = computed(move || connection.state.with(|state| state.file_rendering));
    // The opening whose line the reader left for the rendering, as T3 Code's
    // handled reveal.
    let line_left = signal(None::<u64>);
    let shown = computed(move || {
        file_rendered(
            &path.get(),
            preferences.get(),
            target.with(|target| target.line_request),
            line_left.get(),
        )
    });
    let rendered = computed(move || shown.get().is_some());
    // T3 Code's toggle: pressed, with the source's glyph, while the rendering shows.
    let rendering_toggle = offered.map(|rendering| {
        let (glyph, show, hide) = match rendering {
            FileRendering::Markdown => (
                Icon::EYE,
                tr("Markdownをレンダリング表示"),
                tr("Markdownのソースを表示"),
            ),
            FileRendering::Mermaid => (Icon::EYE, tr("図を表示"), tr("Mermaidのソースを表示")),
            FileRendering::Table => (Icon::TABLE, tr("表を表示"), tr("ソースを表示")),
        };
        ui::toggle_with(
            view()
                .child(when(rendered, || ui::icon(Icon::CODE, 16.0)))
                .child(when(move || !rendered.get(), move || ui::icon(glyph, 16.0))),
            move || SharedString::from(if rendered.get() { hide } else { show }),
            move || rendered.get(),
            move || {
                let next = !rendered.get_untracked();
                // The source's field is made again: it shows the link's line again,
                // and a composition does not outlive the field it was in.
                laid_out.set(false);
                composing.set(false);
                line_left.set(
                    target
                        .with_untracked(|target| target.line_request)
                        .filter(|_| next),
                );
                let preferences = preferences.get_untracked().with(rendering, next);
                connection
                    .application
                    .with(|application| application.set_file_rendering(preferences));
            },
        )
    });
    let contents = computed(move || controls.with(|controls| controls.text.clone()));
    let page = move |content: View| {
        scroll()
            .flex_1()
            .min_h_0()
            // As a conversation's, the selection's actions go while it scrolls.
            .on_scroll(move |_: &ScrollEvent| {
                #[cfg(desktop)]
                surroundings.selection.dismiss();
            })
            .child(content)
    };
    let position = computed(move || {
        (
            target.with(|target| (target.line, target.line_request)),
            controls.with(|controls| controls.loaded_revision.clone()),
            laid_out.get(),
        )
    });
    effect(move || {
        if let ((Some(line), _), Some(_), true) = position.get() {
            let offset = file_line_start_utf16(
                controls.with_untracked(|controls| controls.text.clone()),
                line,
            );
            field.set_selection(offset, offset);
        }
    });
    let reload = move || {
        let modified = controls.with_untracked(|controls| controls.modified);
        spawn(async move {
            if modified
                && !nsh_base::alert_dialog()
                    .title(tr("再読込"))
                    .description(tr("保存していない編集を破棄して再読込しますか？"))
                    .cancel("cancel", tr("キャンセル"))
                    .destructive("reload", tr("編集を破棄して再読込"))
                    .show()
                    .await
                    .is_ok_and(|answer| answer.action.as_deref() == Some("reload"))
            {
                return;
            }
            connection.client.with(|client| {
                client.dispatch(
                    &host.get(),
                    None,
                    Intent::ReadFile(op::ReadFile {
                        path: path.get(),
                        discard_draft: true,
                    }),
                )
            });
        });
    };
    let edit_with_ai = move || {
        connection.client.with(|client| {
            client.dispatch(
                &host.get(),
                None,
                Intent::SetDraftText {
                    thread_id: draft.get(),
                    text: file_edit_prompt(path.get()),
                },
            )
        });
        browser.close();
    };
    let save = move || {
        connection.client.with(|client| {
            client.dispatch(
                &host.get(),
                None,
                Intent::SaveFile(op::SaveFile { path: path.get() }),
            )
        })
    };
    view()
        .flex_1()
        .min_h_0()
        .pb_keyboard()
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_1()
                .h(px(40.0))
                .px_3()
                .border_b_1()
                .border_b_color(colors::border())
                .child(
                    text(move || path.get())
                        .text_size(px(12.0))
                        .text_color(colors::muted())
                        .truncate()
                        .selectable(true)
                        .flex_1()
                        .min_w_0(),
                )
                .child(
                    ui::icon_button(Icon::REFRESH, tr("再読込"), reload).enabled(move || !busy()),
                )
                .child(rendering_toggle)
                .child(ui::icon_button(
                    Icon::SHARE,
                    tr("プレビュー・共有"),
                    move || {
                        preview.load(crate::media::receive(
                            connection.client.get().download(host.get(), path.get()),
                        ))
                    },
                ))
                .child(ui::text_button(tr("AIで編集"), edit_with_ai))
                .child(ui::text_button(tr("保存"), save).enabled(move || {
                    controls.with(|controls| controls.can_save) && !busy() && !composing.get()
                })),
        )
        .child(when(
            move || controls.with(|controls| controls.conflict_notice.is_some()),
            move || {
                notice(move || {
                    controls.with(|controls| {
                        controls
                            .conflict_notice
                            .clone()
                            .map(localized)
                            .unwrap_or_default()
                    })
                })
            },
        ))
        .child(when(busy, || notice(tr("処理中…"))))
        .child(when(
            move || controls.with(|controls| controls.truncated),
            || notice(tr("file.preview.truncated")),
        ))
        .child(when(
            move || shown.get() == Some(FileRendering::Markdown),
            move || {
                // T3 Code's `mx-auto max-w-4xl px-6 py-5`.
                page(
                    view()
                        .w_full()
                        .items_center()
                        .px(px(24.0))
                        .py(px(20.0))
                        .child(crate::rich_text::file(
                            surroundings,
                            path.get(),
                            move || Some(contents.get()),
                            || 896.0,
                        )),
                )
            },
        ))
        .child(when(
            move || shown.get() == Some(FileRendering::Mermaid),
            move || {
                // T3 Code's `MermaidViewer`: the diagram a message would show, or
                // why it is not drawn.
                let diagram = computed(move || contents.with(|text| mermaid_file_markdown(text)));
                page(
                    view()
                        .w_full()
                        .p(px(24.0))
                        .child(when(
                            move || diagram.with(Option::is_none),
                            || {
                                text(tr(
                                    "この図にはリソースのURL、マークアップ、ノードのメタデータ、または独自の設定が含まれています。安全に確認するにはMermaidのソースを表示してください。",
                                ))
                                .text_size(px(12.0))
                                .text_color(colors::muted())
                            },
                        ))
                        .child(crate::rich_text::message(
                            surroundings,
                            move || diagram.get(),
                            || crate::conversation::Kind::Response,
                            || 10_000.0,
                        )),
                )
            },
        ))
        .child(when(
            move || shown.get() == Some(FileRendering::Table),
            move || table::screen(path, move || contents.get()),
        ))
        .child(when(
            move || !rendered.get(),
            move || {
                input()
                    .px_3()
                    .py_2()
                    .multiline(true)
                    .submit_behavior(SubmitBehavior::Newline)
                    .readonly(move || controls.with(|controls| !controls.can_edit))
                    .on_layout(
                        move |_: &nsh_core::geometry::Bounds<nsh_core::geometry::Points>| {
                            laid_out.set(true)
                        },
                    )
                    .value(move || controls.with(|controls| controls.text.clone()))
                    .on_change(move |text: &str| {
                        connection.client.with(|client| {
                            client.dispatch(
                                &host.get(),
                                None,
                                Intent::SetFileDraft {
                                    path: path.get(),
                                    text: text.into(),
                                },
                            )
                        })
                    })
                    .on_composition_change(move |event: &Composition| {
                        composing.set(event.composing)
                    })
                    .auto_correct(false)
                    .auto_capitalize(AutoCapitalize::None)
                    // VoiceOver names the text by its file, as the header shows it.
                    .label({
                        let path = path.get();
                        std::path::Path::new(&path).file_name().map_or_else(
                            || path.clone(),
                            |name| name.to_string_lossy().into_owned(),
                        )
                    })
                    .font_family(SharedString::from("monospace"))
                    .text_size(if is_desktop() { px(14.0) } else { px(17.0) })
                    .text_color(move || nsh_base::theme().foreground)
                    .element_ref(&field)
                    .flex_1()
                    .min_h_0()
                    .w_full()
            },
        ))
        .map(|screen| {
            #[cfg(desktop)]
            let screen = screen.child(surroundings.selection_bar());
            screen
        })
}

pub(crate) fn editors() -> impl Element {
    let browser = use_context::<Browser>().unwrap();
    view().flex_1().min_h_0().child(each(
        move || browser.file.get().into_iter().collect::<Vec<_>>(),
        |target: &TextFile| target.id,
        |target, _| screen(target),
    ))
}
