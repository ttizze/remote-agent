//! Core supplies Markdown semantics; TextKit owns native selection, layout and menus.
use crate::Connection;
use crate::conversation::Kind;
use crate::l10n::tr;
#[cfg(any(desktop, target_os = "ios"))]
use agent_core::{panels::PanelScope, state::Intent, store::SideChatStart};
use agent_core::{
    presentation::markdown::{
        MarkdownAlignment, MarkdownBlock, MarkdownFileReference, MarkdownRun, MarkdownStyle,
    },
    state::DraftKey,
};
use nsh_core::{element::ImageSrc, prelude::*};
use serde_json::{Value, json};
use std::collections::HashMap;

mod search;
pub(crate) use search::documents as search_documents;

/// How far a code block keeps its code from its edges, as T3 Code's `pre`
/// (`0.8rem 0.9rem`) inside its 1 point border.
const CODE_INSET: f32 = 15.0;
/// A table cell's sides, T3 Code's `0.75rem` each.
const CELL_INSET: f32 = 24.0;
/// The widest a table cell grows before its text wraps, T3 Code's `24rem`.
const CELL_MAX: f32 = 384.0;

#[derive(Clone, PartialEq)]
enum Part {
    Text(Vec<Value>),
    Image(String),
    Table {
        source: String,
        csv: String,
        rows: Vec<Vec<Vec<Part>>>,
    },
    Link(String),
    Rule,
    Code {
        label: String,
        source: String,
        blocks: Vec<Value>,
        /// A complete Mermaid block's drawing, shown in place of its source.
        diagram: Option<Diagram>,
    },
}

/// A Mermaid block as Core draws it.
#[derive(Clone, PartialEq)]
struct Diagram {
    svg: String,
    width: f32,
    height: f32,
}

/// What a message's file runs show: the chip each names, if it shows one.
type Chip<'a> = &'a dyn Fn(&MarkdownFileReference) -> Option<Value>;

thread_local! {
    /// Drawn Mermaid blocks by their fenced source: a streamed message's
    /// earlier diagrams are not laid out again for every delta.
    static DIAGRAMS: std::cell::RefCell<Diagrams> = Default::default();
}

/// Drawn Mermaid blocks by their code.
type Diagrams = std::collections::HashMap<String, Option<(String, Diagram)>>;

/// The drawings of `source`'s complete Mermaid blocks, by their code.
fn diagrams(source: &str) -> Vec<(String, Diagram)> {
    use agent_core::presentation::markdown::{markdown_diagram, markdown_mermaid_fences};
    let fences = markdown_mermaid_fences(source);
    if fences.is_empty() {
        return Vec::new();
    }
    DIAGRAMS.with_borrow_mut(|cache| {
        if cache.len() > 64 {
            cache.clear();
        }
        fences
            .into_iter()
            .filter_map(|fence| {
                cache
                    .entry(fence)
                    .or_insert_with_key(|fence| {
                        markdown_diagram(fence).map(|diagram| {
                            let drawing = Diagram {
                                svg: diagram.svg,
                                width: diagram.width,
                                height: diagram.height,
                            };
                            (diagram.source, drawing)
                        })
                    })
                    .clone()
            })
            .collect()
    })
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Target {
    host: String,
    pane: Option<u64>,
    draft: DraftKey,
    storage: String,
    cwd: String,
    home: Option<String>,
    provider: Option<agent_protocol::session::ProviderKind>,
}

/// What message content reaches. List rows are made outside the context
/// their list was made in, so they receive these rather than look them up.
#[derive(Clone, Copy)]
pub(crate) struct Surroundings {
    pub connection: Connection,
    pub files: crate::files::Browser,
    pub preview: crate::media::Preview,
    pub workspace: crate::workbench::Workbench,
    pub find: crate::find::Find,
    pub find_row: Option<Memo<String>>,
    pub find_scroll: crate::find::Viewports,
    /// The bar of actions on the text selected in the list's messages.
    #[cfg(desktop)]
    pub selection: crate::selection_bar::SelectionBar,
}

impl Surroundings {
    /// The surroundings of the list being made, with a selection bar of its own.
    pub(crate) fn current() -> Self {
        Self {
            connection: use_context::<Connection>().unwrap(),
            files: use_context::<crate::files::Browser>().unwrap(),
            preview: use_context::<crate::media::Preview>().unwrap(),
            workspace: use_context::<crate::workbench::Workbench>().unwrap(),
            find: use_context::<crate::find::Find>().unwrap(),
            find_row: None,
            find_scroll: None,
            #[cfg(desktop)]
            selection: crate::selection_bar::SelectionBar::new(),
        }
    }

    /// The selection bar's view: its actions act as the text's own do.
    #[cfg(desktop)]
    pub(crate) fn selection_bar(self) -> impl IntoElement {
        self.selection.view(move |name, selected| {
            action(
                self,
                &json!({"action": name, "text": selected.text, "target": selected.target}),
            )
        })
    }

    /// Opens a link clicked in `target`'s text, `command` (⌘, Ctrl off the
    /// Mac) held or not, as the desktop client's links did: a web page in the
    /// in-app browser with it; on the desktop, a file by its primary action,
    /// which the editor takes with it; anything else as the file browser does.
    fn open_link(self, link: String, target: Target, command: bool) {
        if command && (link.starts_with("https://") || link.starts_with("http://")) {
            return self.workspace.browse(link);
        }
        crate::file_actions::press(self.connection, self.files, target.link(link), command);
    }
}

/// `kind` is whose text it is: the native text names its role for accessibility
/// and dims an activity's.
pub(crate) fn content(
    surroundings: Surroundings,
    blocks: impl Fn() -> Vec<MarkdownBlock> + 'static,
    kind: impl Fn() -> Kind + Copy + 'static,
    max_width: impl Fn() -> f32 + Copy + 'static,
) -> nsh_core::element::View {
    let content = computed(move || parts(blocks(), &[], &|_| None));
    view_parts(
        surroundings,
        move || content.get(),
        target(surroundings.connection),
        kind,
        computed(max_width),
        None,
    )
}

/// A message's Markdown, file chips and diagrams use one presentation on
/// every NSH platform. Only the final native or drawn text adapter differs.
pub(crate) fn message(
    surroundings: Surroundings,
    source: impl Fn() -> Option<String> + 'static,
    kind: impl Fn() -> Kind + Copy + 'static,
    max_width: impl Fn() -> f32 + Copy + 'static,
) -> nsh_core::element::View {
    let connection = surroundings.connection;
    let document = computed(move || {
        let Some(source) = source() else {
            return (Vec::new(), Vec::new());
        };
        let diagrams = diagrams(&source);
        let blocks = agent_core::presentation::markdown::workspace_markdown_blocks(
            source,
            connection
                .composer
                .with(|c| c.controls.navigation.cwd.clone()),
            connection.context.with(|c| c.home()),
        );
        (blocks, diagrams)
    });
    let chips = computed(move || crate::file_actions::Chips::read(connection));
    let paths = computed(move || document.with(|(blocks, _)| file_paths(blocks)));
    effect(move || crate::file_actions::read_metadata(connection, paths.get()));
    let content = computed(move || {
        chips.with(|chips| {
            document.with(|(blocks, diagrams)| {
                search::indexed(parts(
                    blocks.clone(),
                    if surroundings.find.highlighted().is_empty() {
                        diagrams
                    } else {
                        &[]
                    },
                    &|file| Some(chips.chip(file)),
                ))
            })
        })
    });
    view_parts(
        surroundings,
        move || content.get(),
        target(connection),
        kind,
        computed(max_width),
        None,
    )
}

/// Host paths for the document's file metadata, including files in tables.
fn file_paths(blocks: &[MarkdownBlock]) -> Vec<String> {
    blocks
        .iter()
        .flat_map(|block| {
            let (runs, rows) = match block {
                MarkdownBlock::Paragraph { runs, .. } => (runs.as_slice(), &[][..]),
                MarkdownBlock::Table { rows, .. } => (&[][..], rows.as_slice()),
                MarkdownBlock::Visualization { .. } => (&[][..], &[][..]),
            };
            runs.iter()
                .chain(rows.iter().flatten().flat_map(|cell| &cell.runs))
        })
        .filter_map(|run| run.file.as_ref().map(|file| file.path.clone()))
        .collect()
}

/// Where the content's menus and links act: its Host, draft and workspace.
fn target(connection: Connection) -> Memo<String> {
    computed(move || {
        let scope = connection.scope.get();
        let (cwd, provider) = connection
            .composer
            .with(|c| (c.controls.navigation.cwd.clone(), c.controls.provider));
        serde_json::to_string(&Target {
            host: scope.host,
            storage: scope.storage,
            draft: scope.draft,
            pane: connection.pane,
            cwd,
            home: connection.context.with(|c| c.home()),
            provider,
        })
        .expect("display targets contain only serializable values")
    })
}

/// What message text asks for: a selection quoted, explained or asked about
/// in a side chat, a link pressed, or an item of a file chip's menu chosen.
#[cfg(any(desktop, target_os = "ios"))]
fn action(surroundings: Surroundings, data: &Value) {
    let connection = surroundings.connection;
    // TextKit's selection changed: where it is, in the window's content.
    #[cfg(target_os = "macos")]
    if data["action"] == "selection" {
        return surroundings.selection.select(selected(data));
    }
    let Some(text) = data["text"].as_str().filter(|s| !s.is_empty()) else {
        return;
    };
    let Some(target) = data["target"]
        .as_str()
        .and_then(|s| serde_json::from_str::<Target>(s).ok())
    else {
        return;
    };
    match data["action"].as_str() {
        Some("quote") => {
            let visible = connection.host.get_untracked() == target.host
                && connection.pane == target.pane
                && connection
                    .publication
                    .with_untracked(|p| p.composer.controls.navigation.draft_key == target.draft);
            connection.client.with(|client| {
                client.dispatch(
                    &target.host,
                    target.pane,
                    Intent::QuoteSelection {
                        storage_scope: target.storage,
                        thread_id: target.draft,
                        text: text.into(),
                    },
                )
            });
            if visible {
                after_answer(move || connection.composer_ref.focus());
            }
        }
        // From a side chat, it explains in a conversation of its own there.
        Some("explain") if let Some(pane) = target.pane => connection
            .application
            .with(|app| app.explain_in_side_chat(pane, text.into())),
        Some(action @ ("sideChat" | "explain")) => connection.application.with(|app| {
            app.open_side_chat(
                PanelScope {
                    host: target.host,
                    storage: target.storage,
                    draft: target.draft,
                },
                target.cwd,
                target.provider,
                if action == "explain" {
                    SideChatStart::Explain(text.into())
                } else {
                    SideChatStart::Quote(text.into())
                },
            );
        }),
        Some("link") => surroundings.open_link(
            text.into(),
            target,
            data["command"].as_bool().unwrap_or(false),
        ),
        Some("file") => crate::file_actions::choose(
            connection,
            surroundings.files,
            target.link(text.into()),
            data["id"].as_str().unwrap_or_default(),
        ),
        _ => {}
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn native_bounds(data: &Value) -> Option<Bounds<Points>> {
    let [x, y, width, height]: [f32; 4] = serde_json::from_value(data.clone()).ok()?;
    Some(Bounds::new(
        point(px(x), px(y)),
        size(px(width), px(height)),
    ))
}

/// A selection TextKit reported (`"selection"`): its text, its message's
/// target, and its box in the window's content; nothing once it is empty.
#[cfg(target_os = "macos")]
fn selected(data: &Value) -> Option<crate::selection_bar::Selected> {
    let text = data["text"].as_str().filter(|text| !text.is_empty())?;
    Some(crate::selection_bar::Selected {
        text: text.into(),
        target: data["target"].as_str()?.into(),
        bounds: native_bounds(&data["bounds"])?,
        side_chat: data["sideChat"].as_bool().unwrap_or(false),
    })
}

impl Target {
    fn link(self, link: String) -> crate::file_actions::LinkTarget {
        crate::file_actions::LinkTarget {
            link,
            cwd: self.cwd,
            home: self.home,
            host: self.host,
        }
    }
}

/// `cell`: a table's cell, `header` or `body`, whose text is T3 Code's
/// smaller table text.
fn view_parts(
    surroundings: Surroundings,
    parts: impl Fn() -> Vec<Part> + 'static,
    target: Memo<String>,
    kind: impl Fn() -> Kind + Copy + 'static,
    max_width: Memo<f32>,
    cell: Option<&'static str>,
) -> nsh_core::element::View {
    let Surroundings {
        connection, files, ..
    } = surroundings;
    let parts = computed(move || std::sync::Arc::new(parts()));
    let render = move |part: ReadSignal<(usize, Part)>, _: ReadSignal<usize>| {
        part.with_untracked(|(_, value)| match value {
            Part::Rule => view()
                .w_full()
                .h(px(1.0))
                .bg(crate::ui::colors::border())
                .into_element(),
            Part::Text(_) | Part::Code { .. } => {
                // Reused cells lay text out to the lane, not a width measured
                // for another row; unwrapped code keeps its lines.
                #[cfg(any(target_os = "macos", target_os = "ios"))]
                let message = move |wrap: Option<Signal<bool>>, scrolls: crate::find::Viewports, reference: ElementRef| {
                    native("bex.messageText")
                        .element_ref(&reference)
                        .prop("data", move || {
                            // A recycled native cell can finish its pending
                            // update after the message owning these values left
                            // the conversation. It has no document to update.
                            let (Some(width), Some(target)) =
                                (max_width.try_get(), target.try_get())
                            else {
                                return Value::Null;
                            };
                            part.with(|(_, part)| {
                                let code = matches!(part, Part::Code { .. });
                                let (Part::Text(blocks) | Part::Code { blocks, .. }) = part else {
                                    return Value::Null;
                                };
                                let kind = kind();
                                let row = surroundings
                                    .find_row
                                    .map(|row| row.get())
                                    .unwrap_or_default();
                                let document = search::document(blocks);
                                let query = if row.is_empty() {
                                    String::new()
                                } else {
                                    surroundings.find.highlighted()
                                };
                                let active = surroundings.find.active(&row, document);
                                let width = if code && wrap.is_some_and(|wrap| !wrap.get()) {
                                    1_000_000.0
                                } else {
                                    (width - if code { 2.0 * CODE_INSET } else { 0.0 }).max(0.0)
                                };
                                json!({
                                    "findKey": if active.is_some() { surroundings.find.key() } else { None },
                                    "foreground": crate::ui::colors::contrasted(match kind {
                                        Kind::Activity => 0x8c8c8c,
                                        Kind::User => 0xdadada,
                                        _ => 0xd4d4d4,
                                    }),
                                    "blocks": search::decorate(blocks.clone(), &query, active),
                                    "secondary": kind == Kind::Activity,
                                    "role": if kind == Kind::User { "user" } else { "assistant" },
                                    "target": target,
                                    "sideChat": connection.pane.is_none(),
                                    "cell": cell,
                                    "maxWidth": width,
                                    "menu": {
                                        "quote": tr("チャットに追加"),
                                        "sideChat": tr("サイドチャットで質問"),
                                    },
                                })
                            })
                        })
                        .on_native_event(move |event: &NativeEvent| {
                            if let Some(rect) = native_bounds(&event.data["findBounds"]) && let Some(key) = event.data["findKey"].as_str() {
                                surroundings.find.reveal(reference, rect, key.to_owned(), scrolls);
                            } else {
                                action(surroundings, &event.data)
                            }
                        })
                        .min_h(px(20.0))
                        .w_full().into_element()
                };
                #[cfg(not(any(target_os = "macos", target_os = "ios")))]
                let message = move |_: Option<Signal<bool>>, scrolls: crate::find::Viewports, reference: ElementRef| {
                    drawn::message(
                        part,
                        move || kind() == Kind::Activity,
                        target,
                        Surroundings { find_scroll: scrolls, ..surroundings },
                        cell,
                    ).element_ref(&reference).w_full().into_element()
                };
                if matches!(value, Part::Code { .. }) {
                    let selected = computed(move || {
                        let row = surroundings.find_row?.get();
                        let document = part.with(|(_, part)| match part {
                            Part::Code { blocks, .. } => search::document(blocks),
                            _ => 0,
                        });
                        surroundings.find.active(&row, document)?;
                        surroundings.find.key()
                    });
                    frame(part, surroundings.find_scroll, selected, message)
                } else {
                    message(None, surroundings.find_scroll, element_ref())
                }
            }
            Part::Image(_) => crate::media::image_view(
                surroundings.connection,
                surroundings.preview,
                move || {
                    part.with(|(_, part)| {
                        if let Part::Image(source) = part {
                            source.clone()
                        } else {
                            String::new()
                        }
                    })
                },
                crate::media::ImageSize::Natural,
                None,
            )
            .into_element(),
            Part::Link(_) => crate::ui::row_text_button(tr("ファイルを開く"), move || {
                let target = serde_json::from_str::<Target>(&target.get_untracked()).unwrap();
                let Part::Link(path) = part.with_untracked(|(_, part)| part.clone()) else {
                    return;
                };
                files.open(
                    path,
                    target.cwd,
                    target.home,
                    target.host,
                    connection.client.get(),
                );
            })
            .into_element(),
            // T3 Code's table keeps its `0.65rem` margins at the answer's
            // start and end too, where a paragraph's fall away.
            Part::Table { .. } => view()
                .w_full()
                .pt(move || {
                    px(if part.with(|(index, _)| *index == 0) {
                        10.4
                    } else {
                        0.0
                    })
                })
                .pb(move || {
                    let last = Some(part.with(|(index, _)| *index + 1))
                        == parts.try_get().map(|parts| parts.len());
                    px(if last { 10.4 } else { 0.0 })
                })
                .child(table(surroundings, part, target, kind, max_width))
                .into_element(),
        })
    };
    let key = |(index, part): &(usize, Part)| (*index, std::mem::discriminant(part));
    let all = move || {
        each(
            move || parts.with(|parts| parts.iter().cloned().enumerate().collect::<Vec<_>>()),
            key,
            render,
        )
    };
    #[cfg(target_os = "macos")]
    let content = {
        let multiple = move || {
            cell.is_none()
                && !surroundings
                    .find_row
                    .is_some_and(|row| surroundings.find.selected_row(&row.get()))
                && parts.try_get().is_some_and(|parts| parts.len() > 1)
        };
        view()
            .w_full()
            .gap_3()
            .child(when(multiple, move || {
                // A message can contain thousands of code blocks. Recycle
                // its parts against the conversation's viewport. A list has
                // no intrinsic cross-axis size: supply the lane width so a
                // shrink-to-fit user bubble cannot collapse its cells to zero.
                virtual_list(
                    move || parts.try_get().map_or(0, |parts| parts.len()),
                    move |at| {
                        view().w_full().child(each(
                            move || {
                                parts.try_get().map_or_else(Vec::new, |parts| {
                                    parts
                                        .get(at.index())
                                        .cloned()
                                        .map(|part| (at.index(), part))
                                        .into_iter()
                                        .collect::<Vec<_>>()
                                })
                            },
                            key,
                            render,
                        ))
                    },
                )
                .viewport(nsh_core::ListViewport::Parent)
                .estimated_item_size(px(80.0))
                .row_gap(px(12.0))
                .w(move || px(max_width.try_get().unwrap_or_default()))
            }))
            .child(when(move || !multiple(), all))
    };
    #[cfg(not(target_os = "macos"))]
    let content = view().w_full().gap_3().child(all());
    content
}

/// A code block in the desktop client's frame: its language
/// and actions at its top right, its lines wrapped or scrolled sideways. A
/// Mermaid block shows its drawing, at most its own size, until its source is
/// asked for; the drawing enlarges and its source copies.
fn frame(
    part: ReadSignal<(usize, Part)>,
    scrolls: crate::find::Viewports,
    selected: Memo<Option<String>>,
    message: impl Fn(Option<Signal<bool>>, crate::find::Viewports, ElementRef) -> AnyElement
    + Copy
    + 'static,
) -> AnyElement {
    let diagram = computed(move || {
        part.with(|(_, part)| match part {
            Part::Code { diagram, .. } => diagram.clone(),
            _ => None,
        })
    });
    // As T3 Code's, a code block wraps its lines until asked not to.
    let wrap = signal(true);
    let source_shown = signal(false);
    let large = signal(false);
    let drawn = move || diagram.with(Option::is_some) && !source_shown.get();
    // Find opens a diagram's source just as it opens folded message text.
    // A fresh navigation can reveal it again after the reader hides it.
    effect(move || {
        if selected.get().is_some() {
            source_shown.set(true);
        }
    });
    // T3 Code's `pre`: 14 points around the code, inside the block's border.
    let inset = move |content: AnyElement| {
        view()
            .w_full()
            .px(px(CODE_INSET))
            .pt(px(CODE_INSET - 1.0))
            .pb(px(CODE_INSET))
            .child(content)
    };
    view()
        .w_full()
        .rounded(px(8.0))
        .overflow_hidden()
        .bg(crate::conversation::look::CODE)
        .child(code_header(part, diagram, wrap, source_shown, large))
        .child(when(
            move || !drawn() && wrap.get(),
            move || inset(message(Some(wrap), scrolls, element_ref())),
        ))
        .child(when(
            move || !drawn() && !wrap.get(),
            move || {
                let viewport = element_ref();
                let content = element_ref();
                let parents = crate::find::enclosing(scrolls, viewport, content);
                inset(
                    scroll()
                        .horizontal()
                        .element_ref(&viewport)
                        .w_full()
                        .child(message(Some(wrap), parents, content))
                        .into_element(),
                )
            },
        ))
        .child(when(drawn, move || {
            // The drawing keeps its proportions and never grows past its own
            // size, or past twice the height when enlarged.
            let size =
                move || diagram.with(|d| d.as_ref().map_or((1.0, 1.0), |d| (d.width, d.height)));
            let limit = move || {
                let (width, height) = size();
                width
                    * if large.get() {
                        640.0
                    } else {
                        300.0_f32.min(height)
                    }
                    / height
            };
            inset(
                svg(move || {
                    ImageSrc::svg_markup(
                        &diagram.with(|d| d.as_ref().map(|d| d.svg.clone()).unwrap_or_default()),
                    )
                })
                .w_full()
                .max_w(move || px(limit()))
                .aspect_ratio(move || {
                    let (width, height) = size();
                    width / height
                })
                .into_element(),
            )
        }))
        .into_element()
}

/// T3 Code's code block header: the language at the start in small mono
/// type, the block's actions at the end.
fn code_header(
    part: ReadSignal<(usize, Part)>,
    diagram: Memo<Option<Diagram>>,
    wrap: Signal<bool>,
    source_shown: Signal<bool>,
    large: Signal<bool>,
) -> impl IntoElement {
    use crate::conversation::look;
    let toggle = |flag: Signal<bool>| move || flag.update(|flag| *flag = !*flag);
    let mermaid = move || diagram.with(Option::is_some);
    view()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_2()
        .pt(px(7.0))
        .pr(px(7.0))
        .pl(px(13.0))
        .child(
            text(move || {
                part.with(|(_, part)| match part {
                    Part::Code { label, .. } => label.clone(),
                    _ => String::new(),
                })
            })
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
                .child(when(mermaid, move || {
                    crate::ui::row_text_button(
                        move || {
                            SharedString::from(if source_shown.get() {
                                tr("図を表示")
                            } else {
                                tr("ソース")
                            })
                        },
                        toggle(source_shown),
                    )
                }))
                .child(when(
                    move || mermaid() && !source_shown.get(),
                    move || {
                        crate::ui::row_text_button(
                            move || {
                                SharedString::from(if large.get() {
                                    tr("縮小")
                                } else {
                                    tr("拡大")
                                })
                            },
                            toggle(large),
                        )
                    },
                ))
                .child(when(
                    move || !mermaid() || source_shown.get(),
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
                            toggle(wrap),
                        )
                        .text_color(look::muted())
                    },
                ))
                .child(
                    crate::ui::icon_button_sized(
                        crate::ui::Icon::COPY,
                        move || {
                            SharedString::from(if mermaid() {
                                tr("Mermaidをコピー")
                            } else {
                                tr("コードをコピー")
                            })
                        },
                        24.0,
                        12.0,
                        move || {
                            part.with_untracked(|(_, part)| {
                                if let Part::Code { source, .. } = part {
                                    set_clipboard(source.clone());
                                }
                            })
                        },
                    )
                    .text_color(look::muted()),
                ),
        )
}

/// A table as T3 Code's, its rows ruled below; its copy and wrapping
/// actions below it.
fn table(
    surroundings: Surroundings,
    part: ReadSignal<(usize, Part)>,
    target: Memo<String>,
    kind: impl Fn() -> Kind + Copy + 'static,
    max_width: Memo<f32>,
) -> impl IntoElement {
    let viewport = element_ref();
    let content = element_ref();
    let surroundings = Surroundings {
        find_scroll: crate::find::enclosing(surroundings.find_scroll, viewport, content),
        ..surroundings
    };
    let copy = move |markdown: bool| {
        part.with_untracked(|(_, part)| {
            if let Part::Table { source, csv, .. } = part {
                set_clipboard(if markdown {
                    source.clone()
                } else {
                    csv.clone()
                });
            }
        })
    };
    // T3 Code's table: rows ruled below, no frame, the header row's cells a
    // little taller. As its `min-width: max-content` table, each column is as
    // wide as its widest cell, a cell's text wrapping past 24rem, the columns
    // sharing what the lane has left over and the table scrolling sideways
    // past the lane.
    let rule = crate::conversation::look::rule();
    // The text's width in each cell, by row and column, as laid out.
    let measured = signal(HashMap::<(usize, usize), f32>::new());
    let column = move |index: usize| {
        measured.with(|measured| {
            measured
                .iter()
                .filter(|((_, column), _)| *column == index)
                .map(|(_, width)| *width + CELL_INSET)
                .reduce(f32::max)
        })
    };
    let frame = move || {
        view()
            .element_ref(&content)
            .min_w(move || px(max_width.get()))
            .child(each(
                move || {
                    part.with(|(_, part)| {
                        if let Part::Table { rows, .. } = part {
                            rows.clone().into_iter().enumerate().collect()
                        } else {
                            Vec::new()
                        }
                    })
                },
                |(index, _): &(usize, Vec<Vec<Part>>)| *index,
                move |row, _| {
                    let (at, header) = row.with_untracked(|(index, _)| (*index, *index == 0));
                    view()
                        .flex_row()
                        .items_stretch()
                        .border_b_1()
                        .border_b_color(rule)
                        .child(each(
                            move || {
                                row.with(|(_, row)| {
                                    row.clone().into_iter().enumerate().collect::<Vec<_>>()
                                })
                            },
                            |(index, _): &(usize, Vec<Part>)| *index,
                            move |cell, _| {
                                let index = cell.with_untracked(|(index, _)| *index);
                                let width = move || column(index);
                                let cap = CELL_MAX - CELL_INSET;
                                view()
                                    .flex_none()
                                    .items_start()
                                    .style_when(
                                        move || width().is_some(),
                                        move |s| {
                                            s.w(move || px(width().unwrap_or_default()))
                                                .flex_grow(1.0)
                                        },
                                    )
                                    .px_3()
                                    .py(px(if header { 9.0 } else { 7.0 }))
                                    .child(
                                        view()
                                            .flex_none()
                                            .max_w(px(cap))
                                            .on_layout(move |bounds: &Bounds<Points>| {
                                                let laid = bounds.size.width.0;
                                                if measured.with_untracked(|measured| {
                                                    measured.get(&(at, index)) != Some(&laid)
                                                }) {
                                                    measured.update(|measured| {
                                                        measured.insert((at, index), laid);
                                                    });
                                                }
                                            })
                                            .child(view_parts(
                                                surroundings,
                                                move || cell.with(|(_, parts)| parts.clone()),
                                                target,
                                                kind,
                                                computed(move || cap),
                                                Some(if header { "header" } else { "body" }),
                                            )),
                                    )
                            },
                        ))
                },
            ))
    };
    view()
        .w_full()
        .child(
            scroll()
                .horizontal()
                .element_ref(&viewport)
                .w_full()
                .child(frame()),
        )
        // T3 Code's table action below it: copying as Markdown or CSV from a menu.
        .child(
            view().mt(px(2.0)).flex_row().justify_end().child(
                crate::ui::icon_button_sized(
                    crate::ui::Icon::COPY,
                    tr("表をコピー"),
                    24.0,
                    12.0,
                    move || {
                        crate::ui::popup(
                            &serde_json::json!([
                                {"id": "markdown", "title": tr("Markdownをコピー")},
                                {"id": "csv", "title": tr("CSVをコピー")},
                            ]),
                            move |id| copy(id == "markdown"),
                        )
                    },
                )
                .text_color(crate::conversation::look::muted()),
            ),
        )
}

/// Message text NSH lays out where TextKit does not: each paragraph of the
/// native document as selectable styled text, its links opened as TextKit's
/// are. Selection copies; quoting a selection and asking a side chat about it
/// are TextKit's menu, which these texts do not have.
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
mod drawn {
    use super::{Part, Surroundings, Target};
    use crate::ui::colors;
    use nsh_core::{element::rgb, prelude::*};
    use serde_json::Value;

    pub(super) fn message(
        part: ReadSignal<(usize, Part)>,
        secondary: impl Fn() -> bool + Copy + 'static,
        target_json: Memo<String>,
        surroundings: Surroundings,
        cell: Option<&'static str>,
    ) -> View {
        let target = move || serde_json::from_str::<Target>(&target_json.get_untracked()).ok();
        let links = Links {
            open: move |link: String, command: bool| {
                if let Some(target) = target() {
                    surroundings.open_link(link, target, command);
                }
            },
            choose: move |link: String, id: String| {
                if let Some(target) = target() {
                    crate::file_actions::choose(
                        surroundings.connection,
                        surroundings.files,
                        target.link(link),
                        &id,
                    );
                }
            },
        };
        let message = view().w_full().min_h(px(20.0)).gap_2().selectable(true);
        #[cfg(desktop)]
        let message = selection_actions(message, target_json, surroundings);
        message.child(each(
            move || {
                part.with(|(_, part)| match part {
                    Part::Text(blocks) | Part::Code { blocks, .. } => {
                        let row = surroundings
                            .find_row
                            .map(|row| row.get())
                            .unwrap_or_default();
                        let document = super::search::document(blocks);
                        let query = if row.is_empty() {
                            String::new()
                        } else {
                            surroundings.find.highlighted()
                        };
                        super::search::decorate(
                            blocks.clone(),
                            &query,
                            surroundings.find.active(&row, document),
                        )
                        .into_iter()
                        .enumerate()
                        .map(|(index, block)| {
                            let navigation = super::search::reveals(&block)
                                .then(|| surroundings.find.key())
                                .flatten();
                            (index, block, navigation)
                        })
                        .collect()
                    }
                    _ => Vec::new(),
                })
            },
            |(index, block, navigation): &(usize, Value, Option<String>)| {
                (*index, block.to_string(), navigation.clone())
            },
            move |block, _| {
                paragraph(
                    block.get_untracked().1,
                    secondary(),
                    links,
                    cell,
                    surroundings.find,
                    surroundings.find_scroll,
                )
            },
        ))
    }

    /// The selection's actions, as the Mac's text has them: the bar above the
    /// selection, and a right click's menu with them beside Copy and Select
    /// All, which act on the selection.
    #[cfg(desktop)]
    fn selection_actions(message: View, target: Memo<String>, surroundings: Surroundings) -> View {
        use crate::l10n::tr;
        use nsh_core::{
            desktop::MenuRole,
            element::desktop::{MenuItem, popup_menu_at},
        };
        let side_chat = surroundings.connection.pane.is_none();
        let selected = stored(String::new());
        message
            .on_text_selection_change(move |event: &TextSelectionEvent| {
                selected.replace(event.text.clone());
                surroundings.selection.select(
                    event
                        .page_bounds
                        .filter(|_| !event.text.is_empty())
                        .map(|bounds| crate::selection_bar::Selected {
                            text: event.text.clone(),
                            target: target.get_untracked(),
                            bounds,
                            side_chat,
                        }),
                );
            })
            .on_context_menu(move |event: &ContextMenuEvent| {
                let text = selected.get();
                let act = |name: &'static str| {
                    let text = text.clone();
                    move || {
                        super::action(
                            surroundings,
                            &serde_json::json!({
                                "action": name, "text": text, "target": target.get_untracked(),
                            }),
                        )
                    }
                };
                let mut items = Vec::new();
                if !text.is_empty() {
                    items.push(MenuItem::new(tr("チャットに追加")).on_select(act("quote")));
                    items.push(MenuItem::new(tr("詳細を表示")).on_select(act("explain")));
                    if side_chat {
                        items.push(
                            MenuItem::new(tr("サイドチャットで質問")).on_select(act("sideChat")),
                        );
                    }
                    items.push(MenuItem::separator());
                }
                items.push(MenuItem::role(MenuRole::Copy).disabled(text.is_empty()));
                items.push(MenuItem::role(MenuRole::SelectAll));
                surroundings.selection.dismiss();
                drop(popup_menu_at(items, event.page_position));
            })
    }

    /// What a paragraph's links do: `open` a link pressed, ⌘ (Ctrl here)
    /// held or not, and `choose` an item of a file chip's menu.
    #[derive(Clone, Copy)]
    struct Links<O, C> {
        open: O,
        choose: C,
    }

    /// One paragraph of the document `rich_text::paragraph` writes, in a
    /// table's `cell` (`header` or `body`) or not.
    fn paragraph<O, C>(
        block: Value,
        secondary: bool,
        links: Links<O, C>,
        cell: Option<&'static str>,
        find: crate::find::Find,
        scrolls: crate::find::Viewports,
    ) -> View
    where
        O: Fn(String, bool) + Copy + 'static,
        C: Fn(String, String) + Copy + 'static,
    {
        let code = block["code"].as_bool().unwrap_or(false);
        let size = match block["header"].as_u64() {
            Some(1) => 22.0,
            Some(2) => 18.0,
            Some(3) => 16.0,
            _ if code => 13.0,
            _ if cell.is_some() => 12.0,
            _ => 14.0,
        };
        let mut line = text("").text_size(px(size)).text_color(if secondary {
            colors::muted()
        } else {
            colors::foreground()
        });
        if block["header"].is_u64() || cell == Some("header") {
            line = line.font_weight(FontWeight::SEMIBOLD);
        }
        if code {
            line = line.font_family(SharedString::from("monospace"));
        }
        line = match block["alignment"].as_str() {
            Some("center") => line.text_center(),
            Some("right") => line.text_right(),
            _ => line,
        };
        if let Some(marker) = block["marker"].as_str() {
            let marker = serde_json::json!({"text": format!("{marker} "), "highlights": block["markerHighlights"]});
            line = line.child(highlighted_run(&marker, find, scrolls).text_color(colors::muted()));
        }
        for run in block["runs"].as_array().into_iter().flatten() {
            let mut span = highlighted_run(run, find, scrolls);
            if run["strong"].as_bool().unwrap_or(false) {
                span = span.font_weight(FontWeight::BOLD);
            }
            if run["emphasis"].as_bool().unwrap_or(false) {
                span = span.italic();
            }
            if run["strikethrough"].as_bool().unwrap_or(false) {
                span = span.line_through();
            }
            if run["code"].as_bool().unwrap_or(false) && !code {
                span = span
                    .font_family(SharedString::from("monospace"))
                    .bg(colors::RAISED);
            }
            // The app's look is dark: a run's own color is its dark variant.
            if let Some(color) = run["darkColor"]
                .as_u64()
                .and_then(|c| u32::try_from(c).ok())
            {
                span = span.text_color(rgb(color));
            }
            if let Some(link) = run["link"].as_str().map(str::to_owned) {
                if let Some(chip) = run.get("file") {
                    line = line.child(chip_span(span, chip, link, links));
                    continue;
                }
                let open = links.open;
                span = span
                    .text_color(colors::ACCENT)
                    .underline()
                    .on_press(move |event: &PressEvent| open(link.clone(), event.modifiers.ctrl));
            }
            line = line.child(span);
        }
        let depth = block["depth"].as_u64().unwrap_or(0) as f32;
        let quoted = block["quoted"].as_bool().unwrap_or(false);
        view()
            .w_full()
            .pl(px(depth * 16.0 + if quoted { 12.0 } else { 0.0 }))
            .style_when(
                move || quoted,
                |s| s.border_l_1().border_l_color(colors::border()),
            )
            .child(line)
    }

    fn highlighted_run(
        run: &Value,
        find: crate::find::Find,
        scrolls: crate::find::Viewports,
    ) -> Text {
        if run["highlights"].as_array().is_none_or(Vec::is_empty) {
            return text(run["text"].as_str().unwrap_or_default().to_owned());
        }
        let mut span = text("");
        for fragment in super::search::fragments(run) {
            let highlight = fragment.highlighted;
            let active = fragment.active;
            let reveal = fragment.reveal;
            let fragment = text(fragment.text).style_when(
                move || highlight,
                |style| {
                    style
                        .bg(rgb(if active { 0xf97316 } else { 0xf59e0b }))
                        .text_color(rgb(0x000000))
                },
            );
            if reveal {
                // A zero-width inline anchor measures the first selected glyph
                // without changing how the highlighted text wraps.
                let reference = element_ref();
                let key = find.key();
                span = span.child(
                    view()
                        .element_ref(&reference)
                        .w(px(0.0))
                        .h(px(1.0))
                        .on_layout(move |_: &Bounds<Points>| {
                            if let Some(key) = key.clone() {
                                find.reveal(
                                    reference,
                                    Bounds::new(point(px(0.0), px(0.0)), size(px(0.0), px(1.0))),
                                    key,
                                    scrolls,
                                );
                            }
                        }),
                );
            }
            span = span.child(fragment);
        }
        span
    }

    /// A file chip as the desktop client's: its glyph and label in the file's accent, at
    /// medium weight, its full path and size over it, its menu on a secondary
    /// press; a press opens it as a link does.
    fn chip_span<O, C>(label: Text, chip: &Value, link: String, links: Links<O, C>) -> Text
    where
        O: Fn(String, bool) + Copy + 'static,
        C: Fn(String, String) + Copy + 'static,
    {
        let color = chip["darkColor"]
            .as_u64()
            .and_then(|c| u32::try_from(c).ok())
            .map_or(colors::ACCENT, rgb);
        let glyph = if chip["folder"].as_bool().unwrap_or(false) {
            crate::ui::icon(crate::ui::Icon::FOLDER, 14.0).into_element()
        } else {
            image(format!(
                "data:image/png;base64,{}",
                chip["icon"].as_str().unwrap_or_default(),
            ))
            .w(px(14.0))
            .h(px(14.0))
            .into_element()
        };
        let menu = chip["menu"].clone();
        let (open, choose) = (links.open, links.choose);
        let pressed = link.clone();
        text("")
            .text_color(color)
            .font_weight(FontWeight::MEDIUM)
            .tooltip(crate::ui::tooltip(SharedString::from(
                chip["tooltip"].as_str().unwrap_or_default().to_owned(),
            )))
            .child(glyph)
            .child(text(" "))
            .child(label)
            .on_press(move |event: &PressEvent| open(pressed.clone(), event.modifiers.ctrl))
            .on_context_menu(move |_: &ContextMenuEvent| {
                let link = link.clone();
                crate::ui::popup(&menu, move |id: &str| choose(link.clone(), id.into()));
            })
    }
}

/// A paragraph of the native document; a run naming a file carries its chip.
fn paragraph(runs: Vec<MarkdownRun>, style: &MarkdownStyle, chip: Chip) -> Value {
    json!({
        "header": style.header, "marker": style.marker, "code": style.code,
        "quoted": style.quoted, "depth": style.list_depth,
        "alignment": match style.alignment { MarkdownAlignment::Left => "left", MarkdownAlignment::Center => "center", MarkdownAlignment::Right => "right" },
        "runs": runs.into_iter().map(|run| {
            let mut value = json!({
                "text": run.text, "strong": run.strong, "emphasis": run.emphasis,
                "strikethrough": run.strikethrough, "code": run.code, "link": run.link,
                "darkColor": run.dark_color, "lightColor": run.light_color,
            });
            if let Some(chip) = run.file.as_ref().and_then(chip) {
                value["file"] = chip;
            }
            value
        }).collect::<Vec<_>>()
    })
}

fn flush(text: &mut Vec<Value>, parts: &mut Vec<Part>) {
    if !text.is_empty() {
        parts.push(Part::Text(std::mem::take(text)));
    }
}

/// A code block, labelled with its file or language; a Mermaid block with
/// its drawing when Core drew it.
fn code(
    runs: Vec<MarkdownRun>,
    style: MarkdownStyle,
    diagrams: &[(String, Diagram)],
    chip: Chip,
) -> Part {
    let source: String = runs.iter().map(|run| run.text.as_str()).collect();
    let diagram = (style.language.as_deref() == Some("mermaid"))
        .then(|| {
            diagrams
                .iter()
                .find(|(code, _)| code.trim_end() == source.trim_end())
        })
        .flatten()
        .map(|(_, diagram)| diagram.clone());
    Part::Code {
        label: style
            .filename
            .clone()
            .or_else(|| style.language.clone())
            .unwrap_or_else(|| "CODE".into()),
        source,
        blocks: vec![paragraph(runs, &style, chip)],
        diagram,
    }
}

/// `blocks` as the parts views lay out, a Mermaid block with its drawing.
fn parts(blocks: Vec<MarkdownBlock>, diagrams: &[(String, Diagram)], chip: Chip) -> Vec<Part> {
    let mut result = Vec::new();
    let mut text = Vec::new();
    for block in blocks {
        match block {
            MarkdownBlock::Paragraph { style, .. } if style.rule => {
                flush(&mut text, &mut result);
                result.push(Part::Rule);
            }
            MarkdownBlock::Paragraph { runs, style } if style.code => {
                flush(&mut text, &mut result);
                result.push(code(runs, style, diagrams, chip));
            }
            MarkdownBlock::Paragraph { runs, style } => {
                let mut content = Vec::new();
                for run in runs {
                    if let Some(image) = run.image {
                        if !content.is_empty() {
                            text.push(paragraph(std::mem::take(&mut content), &style, chip));
                        }
                        flush(&mut text, &mut result);
                        result.push(Part::Image(image));
                    } else {
                        content.push(run);
                    }
                }
                if !content.is_empty() {
                    text.push(paragraph(content, &style, chip));
                }
            }
            MarkdownBlock::Visualization { path } => {
                flush(&mut text, &mut result);
                result.push(Part::Link(path));
            }
            MarkdownBlock::Table {
                source,
                columns,
                rows,
            } => {
                flush(&mut text, &mut result);
                result.push(Part::Table {
                    source,
                    csv: agent_core::presentation::markdown::markdown_table_csv(
                        rows.iter()
                            .map(|row| {
                                row.iter()
                                    .map(|cell| {
                                        cell.runs.iter().map(|run| run.text.as_str()).collect()
                                    })
                                    .collect()
                            })
                            .collect(),
                    ),
                    rows: rows
                        .into_iter()
                        .map(|row| {
                            row.into_iter()
                                .enumerate()
                                .map(|(column, cell)| {
                                    parts(
                                        vec![MarkdownBlock::Paragraph {
                                            runs: cell.runs,
                                            style: MarkdownStyle {
                                                alignment: columns
                                                    .get(column)
                                                    .copied()
                                                    .unwrap_or_default(),
                                                ..Default::default()
                                            },
                                        }],
                                        diagrams,
                                        chip,
                                    )
                                })
                                .collect()
                        })
                        .collect(),
                });
            }
        }
    }
    flush(&mut text, &mut result);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_metadata_covers_workspace_links_code_paths_and_table_cells() {
        let blocks = agent_core::presentation::markdown::workspace_markdown_blocks(
            "[統合前レビュー](src/SKILL.md) と `src/main.rs`、`81196806c`\n\n|file|\n|---|\n|[日本語](src/日本語.rs:4)|".into(),
            "/repo".into(),
            None,
        );
        assert_eq!(
            file_paths(&blocks),
            [
                "/repo/src/SKILL.md",
                "/repo/src/main.rs",
                "/repo/src/日本語.rs"
            ]
        );
    }

    #[test]
    fn native_text_contract_preserves_unicode_style_links_and_selection_target() {
        let blocks = vec![paragraph(
            vec![
                MarkdownRun {
                    text: "日本語 👋".into(),
                    strong: true,
                    ..Default::default()
                },
                MarkdownRun {
                    text: " ファイル".into(),
                    link: Some("file:///fixture/%E6%97%A5%E6%9C%AC%E8%AA%9E.txt".into()),
                    ..Default::default()
                },
            ],
            &MarkdownStyle {
                header: Some(2),
                ..Default::default()
            },
            &|_| None,
        )];
        let expected: Value = serde_json::from_str(include_str!(
            "../apple/Tests/BexNshNativeTests/Fixtures/message.json"
        ))
        .unwrap();
        assert_eq!(
            json!({"blocks": blocks, "secondary": false, "role": "user", "target": "opaque-native-menu-target", "sideChat": true, "cell": null, "maxWidth": 400,
                "menu": {"quote": "チャットに追加", "sideChat": "サイドチャットで質問"}}),
            expected
        );
    }

    #[test]
    fn native_parts_preserve_media_code_rules_and_table_copy_sources_in_order() {
        let source = "前文 **強調**\n\n![画像](https://example.invalid/image.png)\n\n```rust title=main.rs\nlet 日本語 = 1;\n```\n\n---\n\n|名前|値|\n|:---|---:|\n|日本語|a,b|";
        let parts = parts(
            agent_core::presentation::markdown::markdown_blocks(source.into()),
            &[],
            &|_| None,
        );
        assert!(matches!(&parts[0], Part::Text(blocks) if blocks[0]["runs"][1]["strong"] == true));
        assert!(
            matches!(&parts[1], Part::Image(source) if source == "https://example.invalid/image.png")
        );
        assert!(matches!(&parts[2], Part::Code { source, .. } if source == "let 日本語 = 1;"));
        assert!(matches!(&parts[3], Part::Rule));
        let Part::Table { source, csv, rows } = &parts[4] else {
            panic!("table is retained");
        };
        assert!(source.starts_with("|名前|値|"));
        assert_eq!(csv, "\"名前\",\"値\"\r\n\"日本語\",\"a,b\"");
        assert_eq!(rows.len(), 2);
        assert!(
            matches!(&rows[1][1][0], Part::Text(blocks) if blocks[0]["alignment"] == "right" && blocks[0]["runs"][0]["text"] == "a,b")
        );
        assert_eq!(parts.len(), 5);
    }

    #[test]
    fn complete_mermaid_blocks_draw_and_a_streaming_one_stays_code() {
        let source = "図です。\n\n```mermaid\nflowchart LR\n  A --> B\n```";
        let parsed = |source: &str| {
            parts(
                agent_core::presentation::markdown::markdown_blocks(source.into()),
                &diagrams(source),
                &|_| None,
            )
        };
        let drawn = parsed(source);
        let Some(Part::Code {
            diagram: Some(diagram),
            source: code,
            ..
        }) = drawn.last()
        else {
            panic!("a complete Mermaid block draws: {}", drawn.len());
        };
        assert!(diagram.svg.contains("<svg") && diagram.width > 0.0 && diagram.height > 0.0);
        assert_eq!(code.trim_end(), "flowchart LR\n  A --> B");
        let streaming = parsed(&source[..source.len() - 3]);
        assert!(
            matches!(streaming.last(), Some(Part::Code { diagram: None, .. })),
            "an unfinished fence is still being written"
        );
    }

    #[test]
    fn a_file_run_carries_the_chip_its_reference_shows() {
        let file = MarkdownFileReference {
            path: "/repo/a.rs".into(),
            label: "a.rs".into(),
            kind: agent_core::presentation::markdown::MarkdownFileKind::Code,
            line: None,
            column: None,
        };
        let block = paragraph(
            vec![
                MarkdownRun {
                    text: "a.rs".into(),
                    link: Some("a.rs".into()),
                    file: Some(file),
                    ..Default::default()
                },
                MarkdownRun {
                    text: " です".into(),
                    ..Default::default()
                },
            ],
            &MarkdownStyle::default(),
            &|file| Some(json!({"tooltip": file.path})),
        );
        assert_eq!(block["runs"][0]["file"]["tooltip"], "/repo/a.rs");
        assert!(block["runs"][1].get("file").is_none());
    }

    #[test]
    fn invocation_and_currency_text_keeps_the_same_gfm_meaning_as_existing_clients() {
        let text = "$fixture-review @Fixture Plugin [success] costs $5 · 日本語 👋";
        let parts = parts(
            agent_core::presentation::markdown::markdown_blocks(text.into()),
            &[],
            &|_| None,
        );
        let [Part::Text(blocks)] = &parts[..] else {
            panic!("plain text stays text");
        };
        assert_eq!(blocks[0]["runs"][0]["text"], text);
        assert_eq!(blocks[0]["runs"][0]["link"], Value::Null);
        assert_eq!(blocks[0]["code"], false);
    }

    #[test]
    fn tool_output_stays_literal_in_the_native_document() {
        let output = "# output\n[open](https://example.com)\n<img src=remote>\n```\n日本語";
        let parts = parts(
            vec![agent_core::presentation::markdown::markdown_code_block(
                output.into(),
                None,
                Some("出力".into()),
            )],
            &[],
            &|_| None,
        );
        let [
            Part::Code {
                blocks,
                source,
                label,
                diagram: None,
            },
        ] = &parts[..]
        else {
            panic!("tool output stays code");
        };
        assert_eq!(source, output);
        assert_eq!(label, "出力");
        assert_eq!(
            blocks[0]["runs"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|run| run["text"].as_str())
                .collect::<String>(),
            output
        );
        assert_eq!(blocks[0]["code"], true);
    }
}
