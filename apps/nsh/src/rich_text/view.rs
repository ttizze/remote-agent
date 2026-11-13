mod document;

use {
    super::{Kind, MarkdownBlock, Part, Surroundings, Target},
    crate::l10n::tr,
    nsh_base::{
        TextViewLinkPressEvent, text_view,
        text_view::{CodeBlock, CustomNode},
    },
    nsh_core::{element::ImageSrc, prelude::*},
    serde_json::Value,
};

pub(crate) fn message(
    surroundings: Surroundings,
    source: impl Fn() -> Option<String> + 'static,
    kind: impl Fn() -> Kind + Copy + 'static,
    max_width: impl Fn() -> f32 + Copy + 'static,
) -> View {
    let parts = super::message_parts(surroundings, source);
    render_parts(surroundings, move || parts.get(), kind, max_width)
}

pub(crate) fn content(
    surroundings: Surroundings,
    blocks: impl Fn() -> Vec<MarkdownBlock> + 'static,
    kind: impl Fn() -> Kind + Copy + 'static,
    max_width: impl Fn() -> f32 + Copy + 'static,
) -> View {
    render_parts(
        surroundings,
        move || super::search::indexed(super::parts(blocks(), &[], &|_| None)),
        kind,
        max_width,
    )
}

fn render_parts(
    surroundings: Surroundings,
    parts: impl Fn() -> Vec<Part> + 'static,
    kind: impl Fn() -> Kind + Copy + 'static,
    max_width: impl Fn() -> f32 + Copy + 'static,
) -> View {
    let target = super::target(surroundings.connection);
    let wraps = signal(std::collections::HashMap::<String, bool>::new());
    let owner = Owner::current();
    let diagrams = stored(std::collections::HashMap::<String, DiagramControls>::new());
    let render = move |node: &CustomNode| {
        let controls = node.attributes.get("document").map(|id| {
            diagrams.with(|all| {
                *all.entry(id.clone())
                    .or_insert_with(|| owner.run(DiagramControls::new))
            })
        });
        custom(node, surroundings, target, controls)
    };
    let document = computed(move || {
        let row = surroundings
            .find_row
            .and_then(|row| row.try_get())
            .unwrap_or_default();
        let query = if row.is_empty() {
            String::new()
        } else {
            surroundings.find.highlighted()
        };
        let active = surroundings.find.selection(&row);
        let navigation = active.and_then(|_| surroundings.find.key());
        document::presentation(parts(), &query, active, navigation.as_deref(), &wraps.get())
    });
    let message = view()
        .w_full()
        .max_w(move || px(max_width()))
        .min_w_0()
        .style_when(move || kind() == Kind::Activity, |s| s.opacity(0.75));
    #[cfg(desktop)]
    let message = super::selection_actions(message.selectable(true), target, surroundings);
    message.child(
        text_view()
            .document(move || document.get())
            .w_full()
            .min_w_0()
            .selectable(true)
            .on_selection_change(move |event: &nsh_base::TextViewSelectionEvent| {
                #[cfg(target_os = "ios")]
                if let Some(actions) = surroundings.external_actions {
                    actions.emit(
                        serde_json::json!({"action":"selection", "text":event.text.to_string()}),
                    );
                }
                #[cfg(not(target_os = "ios"))]
                let _ = event;
            })
            .render_code_block_actions(move |code: &CodeBlock| {
                code_actions(code, wraps, surroundings.find)
            })
            .on_link_press(move |event: &TextViewLinkPressEvent| {
                if event.url.starts_with('#') {
                    return;
                }
                if let Ok(target) = serde_json::from_str::<Target>(&target.get_untracked()) {
                    surroundings.open_link(
                        event.url.to_string(),
                        target,
                        crate::file_actions::primary_modifier(event.modifiers, apple_keys()),
                    );
                }
            })
            .render_custom(render)
            .render_inline_custom(render),
    )
}

fn custom(
    node: &CustomNode,
    surroundings: Surroundings,
    target: Memo<String>,
    controls: Option<DiagramControls>,
) -> AnyElement {
    match node.name.as_str() {
        "bex:image" => {
            let source = node.data.as_str().unwrap_or_default().to_owned();
            crate::media::image_view(
                surroundings.connection,
                surroundings.preview,
                move || source.clone(),
                crate::media::ImageSize::Natural,
                None,
            )
            .into_element()
        }
        "bex:table-actions" => table_actions(&node.data),
        "bex:diagram" => match controls {
            Some(controls) => diagram(&node.data, surroundings, target, controls),
            None => view().into_element(),
        },
        "bex:file" | "bex:link" => {
            let mut label = run(&node.data, surroundings);
            if node.name == "bex:link" && node.data["code"] == true {
                label = label
                    .font_family(SharedString::from("monospace"))
                    .bg(crate::ui::colors::RAISED);
            }
            let resolve = move || serde_json::from_str::<Target>(&target.get_untracked()).ok();
            let links = super::inline::Links {
                open: move |link, command| {
                    if let Some(target) = resolve() {
                        surroundings.open_link(link, target, command);
                    }
                },
                choose: move |link, id: String| {
                    if let Some(target) = resolve() {
                        surroundings.choose_link(link, target, &id);
                    }
                },
            };
            let link = node.data["link"].as_str().unwrap_or_default().into();
            if node.name == "bex:link" {
                super::inline::web_span(label, link, node.data["webMenu"].clone(), links)
                    .into_element()
            } else {
                super::inline::chip_span(label, &node.data["file"], link, links).into_element()
            }
        }
        "bex:code-run" => run(&node.data, surroundings)
            .font_family(SharedString::from("monospace"))
            .bg(crate::ui::colors::RAISED)
            .into_element(),
        "bex:run" => run(&node.data, surroundings).into_element(),
        _ => text(node.text.clone()).into_element(),
    }
}

fn table_actions(data: &Value) -> AnyElement {
    let markdown = data["markdown"].as_str().unwrap_or_default().to_owned();
    let csv = data["csv"].as_str().unwrap_or_default().to_owned();
    view()
        .w_full()
        .flex_row()
        .justify_end()
        .child(
            crate::ui::icon_button_sized(
                crate::ui::Icon::COPY,
                tr("表をコピー"),
                24.0,
                12.0,
                move || {
                    let markdown = markdown.clone();
                    let csv = csv.clone();
                    crate::ui::popup(
                        &serde_json::json!([
                            {"id":"markdown", "title":tr("Markdownをコピー")},
                            {"id":"csv", "title":tr("CSVをコピー")}
                        ]),
                        move |id| {
                            set_clipboard(if id == "markdown" {
                                markdown.clone()
                            } else {
                                csv.clone()
                            })
                        },
                    );
                },
            )
            .text_color(crate::conversation::look::muted()),
        )
        .into_element()
}

fn run(run: &Value, surroundings: Surroundings) -> Text {
    let text = super::inline::highlighted_run(run, surroundings.find);
    match super::run_color(run) {
        Some(color) => text.text_color(color),
        None => text,
    }
}

fn diagram(
    data: &Value,
    surroundings: Surroundings,
    target: Memo<String>,
    controls: DiagramControls,
) -> AnyElement {
    let Ok(Part::Code {
        label,
        source: code,
        blocks,
        diagram: Some(drawing),
    }) = serde_json::from_value::<Part>(data.clone())
    else {
        return view().into_element();
    };
    let DiagramControls {
        wrap,
        source_shown,
        large,
    } = controls;
    let document = super::search::document(&blocks);
    let selected = computed(move || {
        let row = surroundings.find_row?.try_get()?;
        surroundings.find.active(&row, document)?;
        surroundings.find.key()
    });
    effect(move || {
        if selected.get().is_some() {
            source_shown.set(true);
        }
    });
    let (width, height) = (drawing.width, drawing.height);
    let unwrapped_blocks = blocks.clone();
    let inset = |content: AnyElement| {
        view()
            .w_full()
            .px(px(15.0))
            .pt(px(14.0))
            .pb(px(15.0))
            .child(content)
    };
    view()
        .w_full()
        .rounded(px(8.0))
        .overflow_hidden()
        .bg(crate::conversation::look::CODE)
        .child(diagram_header(label, code, controls, surroundings.find))
        .child(when(
            move || source_shown.get() && wrap.get(),
            move || inset(source(surroundings, blocks.clone(), target)),
        ))
        .child(when(
            move || source_shown.get() && !wrap.get(),
            move || {
                inset(
                    scroll()
                        .horizontal()
                        .w_full()
                        .child(source(surroundings, unwrapped_blocks.clone(), target))
                        .into_element(),
                )
            },
        ))
        .child(when(
            move || !source_shown.get(),
            move || {
                inset(
                    svg(ImageSrc::svg_markup(&drawing.svg))
                        .w_full()
                        .max_w(move || {
                            px(width
                                * if large.get() {
                                    640.0
                                } else {
                                    300.0_f32.min(height)
                                }
                                / height)
                        })
                        .aspect_ratio(width / height)
                        .into_element(),
                )
            },
        ))
        .into_element()
}

fn code_actions(
    code: &CodeBlock,
    wraps: Signal<std::collections::HashMap<String, bool>>,
    find: crate::find::Find,
) -> View {
    let id = code.id.clone().unwrap_or_default();
    let wrap = code.wrap;
    let source = code.code.clone();
    view()
        .flex_row()
        .gap(px(2.0))
        .text_color(crate::conversation::look::muted())
        .child(crate::ui::icon_button_sized(
            crate::ui::Icon::WRAP,
            crate::l10n::tr(if wrap {
                "折り返し解除"
            } else {
                "折り返し"
            }),
            24.0,
            12.0,
            move || {
                find.reset_anchor();
                wraps.update(|values| {
                    values.insert(id.clone(), !wrap);
                })
            },
        ))
        .child(crate::ui::icon_button_sized(
            crate::ui::Icon::COPY,
            crate::l10n::tr("コードをコピー"),
            24.0,
            12.0,
            move || set_clipboard(source.clone()),
        ))
}

#[derive(Clone, Copy)]
struct DiagramControls {
    wrap: Signal<bool>,
    source_shown: Signal<bool>,
    large: Signal<bool>,
}

impl DiagramControls {
    fn new() -> Self {
        Self {
            wrap: signal(true),
            source_shown: signal(false),
            large: signal(false),
        }
    }
}

/// Mermaid keeps its drawing controls; TextView owns its source text.
fn diagram_header(
    label: String,
    source: String,
    controls: DiagramControls,
    find: crate::find::Find,
) -> impl IntoElement {
    use crate::conversation::look;
    let DiagramControls {
        wrap,
        source_shown,
        large,
    } = controls;
    view()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_2()
        .pt(px(7.0))
        .pr(px(7.0))
        .pl(px(13.0))
        .child(
            text(label)
                .max_w(px(220.0))
                .number_of_lines(1)
                .font_family(SharedString::from("monospace"))
                .text_size(px(11.0))
                .text_color(look::foreground().alpha(0.72)),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(2.0))
                .text_color(look::muted())
                .child(crate::ui::row_text_button(
                    move || {
                        SharedString::from(if source_shown.get() {
                            tr("図を表示")
                        } else {
                            tr("ソース")
                        })
                    },
                    move || source_shown.update(|shown| *shown = !*shown),
                ))
                .child(when(
                    move || !source_shown.get(),
                    move || {
                        crate::ui::row_text_button(
                            move || {
                                SharedString::from(if large.get() {
                                    tr("縮小")
                                } else {
                                    tr("拡大")
                                })
                            },
                            move || large.update(|large| *large = !*large),
                        )
                    },
                ))
                .child(when(
                    move || source_shown.get(),
                    move || {
                        crate::ui::icon_button_sized(
                            crate::ui::Icon::WRAP,
                            move || {
                                SharedString::from(if wrap.get() {
                                    tr("折り返し解除")
                                } else {
                                    tr("折り返し")
                                })
                            },
                            24.0,
                            12.0,
                            move || {
                                find.reset_anchor();
                                wrap.update(|wrap| *wrap = !*wrap);
                            },
                        )
                    },
                ))
                .child(crate::ui::icon_button_sized(
                    crate::ui::Icon::COPY,
                    tr("Mermaidをコピー"),
                    24.0,
                    12.0,
                    move || set_clipboard(source.clone()),
                )),
        )
}

fn source(surroundings: Surroundings, blocks: Vec<Value>, target: Memo<String>) -> AnyElement {
    let document = computed(move || {
        let row = surroundings
            .find_row
            .and_then(|row| row.try_get())
            .unwrap_or_default();
        let query = if row.is_empty() {
            String::new()
        } else {
            surroundings.find.highlighted()
        };
        let active = surroundings.find.selection(&row);
        let navigation = active.and_then(|_| surroundings.find.key());
        document::presentation(
            vec![Part::Text(blocks.clone())],
            &query,
            active,
            navigation.as_deref(),
            &Default::default(),
        )
    });
    text_view()
        .document(move || document.get())
        .w_full()
        .min_w_0()
        .font_family(SharedString::from("monospace"))
        .text_size(px(13.0))
        .selectable(true)
        .on_selection_change(move |event: &nsh_base::TextViewSelectionEvent| {
            #[cfg(target_os = "ios")]
            if let Some(actions) = surroundings.external_actions {
                actions
                    .emit(serde_json::json!({"action":"selection", "text":event.text.to_string()}));
            }
            #[cfg(not(target_os = "ios"))]
            let _ = event;
        })
        .render_inline_custom(move |node: &CustomNode| custom(node, surroundings, target, None))
        .into_element()
}
