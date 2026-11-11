//! One standard TextView per message, using Core's shared workspace semantics.
//! Only application-specific chips, diagrams and search spans use custom nodes;
//! TextView owns paragraph, code and table layout and the timeline owns scrolling.
#[cfg(any(test, all(target_os = "macos", debug_assertions)))]
mod document;

#[cfg(all(target_os = "macos", debug_assertions))]
use {
    super::{Kind, MarkdownBlock, Part, Surroundings, Target},
    nsh_base::{
        TextViewLinkPressEvent, text_view,
        text_view::{CodeBlock, CustomNode},
    },
    nsh_core::prelude::*,
    serde_json::Value,
};

#[cfg(all(target_os = "macos", debug_assertions))]
pub(super) fn message(
    surroundings: Surroundings,
    source: impl Fn() -> Option<String> + 'static,
    kind: impl Fn() -> Kind + Copy + 'static,
    max_width: impl Fn() -> f32 + Copy + 'static,
) -> View {
    let parts = super::message_parts(surroundings, source);
    render_parts(surroundings, move || parts.get(), kind, max_width)
}

#[cfg(all(target_os = "macos", debug_assertions))]
pub(super) fn content(
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

#[cfg(all(target_os = "macos", debug_assertions))]
fn render_parts(
    surroundings: Surroundings,
    parts: impl Fn() -> Vec<Part> + 'static,
    kind: impl Fn() -> Kind + Copy + 'static,
    max_width: impl Fn() -> f32 + Copy + 'static,
) -> View {
    let target = super::target(surroundings.connection);
    let wraps = signal(std::collections::HashMap::<String, bool>::new());
    let owner = Owner::current();
    let diagrams = stored(std::collections::HashMap::<String, super::CodeControls>::new());
    let render = move |node: &CustomNode| {
        let controls = node.attributes.get("document").map(|id| {
            diagrams.with(|all| {
                *all.entry(id.clone())
                    .or_insert_with(|| owner.run(super::CodeControls::new))
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
    super::selection_actions(
        view()
            .w_full()
            .max_w(move || px(max_width()))
            .min_w_0()
            .selectable(true)
            .style_when(move || kind() == Kind::Activity, |s| s.opacity(0.75)),
        target,
        surroundings,
    )
    .child(
        text_view()
            .document(move || document.get())
            .w_full()
            .min_w_0()
            .selectable(true)
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

#[cfg(all(target_os = "macos", debug_assertions))]
fn custom(
    node: &CustomNode,
    surroundings: Surroundings,
    target: Memo<String>,
    controls: Option<super::CodeControls>,
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
        "bex:diagram" => match controls {
            Some(controls) => diagram(&node.data, surroundings, target, controls),
            None => view().into_element(),
        },
        "bex:file" => {
            let label = run(&node.data, surroundings);
            let resolve = move || serde_json::from_str::<Target>(&target.get_untracked()).ok();
            super::drawn::chip_span(
                label,
                &node.data["file"],
                node.data["link"].as_str().unwrap_or_default().into(),
                super::drawn::Links {
                    open: move |link, command| {
                        if let Some(target) = resolve() {
                            surroundings.open_link(link, target, command);
                        }
                    },
                    choose: move |link, id: String| {
                        if let Some(target) = resolve() {
                            crate::file_actions::choose(
                                surroundings.connection,
                                surroundings.files,
                                target.link(link),
                                &id,
                            );
                        }
                    },
                },
            )
            .into_element()
        }
        "bex:code-run" => run(&node.data, surroundings)
            .font_family(SharedString::from("monospace"))
            .bg(crate::ui::colors::RAISED)
            .into_element(),
        "bex:run" => run(&node.data, surroundings).into_element(),
        _ => text(node.text.clone()).into_element(),
    }
}

#[cfg(all(target_os = "macos", debug_assertions))]
fn run(run: &Value, surroundings: Surroundings) -> Text {
    let text = super::drawn::highlighted_run(run, surroundings.find, None, true);
    match run["darkColor"]
        .as_u64()
        .and_then(|c| u32::try_from(c).ok())
    {
        Some(color) => text.text_color(rgb(color)),
        None => text,
    }
}

#[cfg(all(target_os = "macos", debug_assertions))]
fn diagram(
    data: &Value,
    surroundings: Surroundings,
    target: Memo<String>,
    controls: super::CodeControls,
) -> AnyElement {
    let Ok(part) = serde_json::from_value::<Part>(data.clone()) else {
        return view().into_element();
    };
    let part = signal((0, part));
    let selected = computed(move || {
        let row = surroundings.find_row?.try_get()?;
        let document = part.with(|(_, part)| match part {
            Part::Code { blocks, .. } => super::search::document(blocks),
            _ => 0,
        });
        surroundings.find.active(&row, document)?;
        surroundings.find.key()
    });
    super::frame(
        part.read_only(),
        surroundings.find_scroll,
        selected,
        controls,
        move |_, scrolls, _| {
            super::drawn::message(
                part.read_only(),
                || false,
                target,
                Surroundings {
                    find_scroll: scrolls,
                    ..surroundings
                },
                None,
            )
            .into_element()
        },
    )
}

#[cfg(all(target_os = "macos", debug_assertions))]
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
