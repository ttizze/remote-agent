//! Core supplies Markdown semantics; NSH TextView owns message layout and selection.
use crate::Connection;
use crate::conversation::Kind;
#[cfg(desktop)]
use crate::l10n::tr;
#[cfg(desktop)]
use agent_core::{panels::PanelScope, state::Intent, store::SideChatStart};
use agent_core::{
    presentation::markdown::{
        MarkdownAlignment, MarkdownBlock, MarkdownFileReference, MarkdownRun, MarkdownStyle,
    },
    presentation::web_links::{WebLinkAction, WebLinkTarget, web_link_menu, web_link_target},
    state::DraftKey,
};
use nsh_core::prelude::*;
use serde_json::{Value, json};

mod search;
mod view;
pub(crate) use search::documents as search_documents;
pub(crate) use view::{content, message};

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
enum Part {
    Text(Vec<Value>),
    Image {
        source: String,
        depth: u32,
        marker: Option<String>,
    },
    Table {
        source: String,
        csv: String,
        rows: Vec<Vec<Vec<Part>>>,
        depth: u32,
        marker: Option<String>,
    },
    Link {
        url: String,
        depth: u32,
        marker: Option<String>,
    },
    Rule {
        depth: u32,
        marker: Option<String>,
    },
    Code {
        label: String,
        source: String,
        blocks: Vec<Value>,
        /// A complete Mermaid block's drawing, shown in place of its source.
        diagram: Option<Diagram>,
    },
}

/// A Mermaid block as Core draws it.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
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

/// Preserve both appearances when Core supplies an annotated run's colors.
fn run_color(run: &Value) -> Option<AdaptiveColor> {
    let color = |key: &str| run[key].as_u64().and_then(|c| u32::try_from(c).ok());
    let light = color("lightColor");
    let dark = color("darkColor");
    Some(AdaptiveColor::rgb(light.or(dark)?, dark.or(light)?))
}

/// What message content reaches. List rows are made outside the context
/// their list was made in, so they receive these rather than look them up.
#[derive(Clone, Copy)]
pub(crate) struct Surroundings {
    pub connection: Connection,
    pub files: crate::files::Browser,
    pub preview: crate::media::Preview,
    pub workspace: crate::workbench::Workbench,
    #[cfg(any(desktop, target_os = "ios"))]
    pub external_actions: Option<crate::native::ExternalActions>,
    pub find: crate::find::Find,
    pub find_row: Option<Memo<String>>,
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
            #[cfg(any(desktop, target_os = "ios"))]
            external_actions: use_context::<crate::native::ExternalActions>(),
            find: use_context::<crate::find::Find>().unwrap(),
            find_row: None,
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

    /// Web pages follow this device's preference; ⌘/Ctrl opens them externally.
    /// Files retain their primary action and modifier shortcut to the editor.
    fn open_link(self, link: String, target: Target, command: bool) {
        #[cfg(any(desktop, target_os = "ios"))]
        if let Some(actions) = self.external_actions {
            return actions.emit(json!({"action":"link", "text":link}));
        }
        let preference = self
            .connection
            .publication
            .with_untracked(|p| p.snapshot.catalog.web_link_target);
        if let Some(destination) = web_link_target(&link, preference, command) {
            return self.open_web_link(link, destination);
        }
        crate::file_actions::press(self.connection, self.files, target.link(link), command);
    }

    fn open_web_link(self, link: String, destination: WebLinkTarget) {
        match destination {
            WebLinkTarget::SystemBrowser => crate::native::open_link(link),
            WebLinkTarget::InAppBrowser => self.workspace.browse(link),
        }
    }

    fn choose_link(self, link: String, target: Target, id: &str) {
        if web_link_target(&link, WebLinkTarget::SystemBrowser, false).is_some()
            && let Ok(action) = serde_json::from_value::<WebLinkAction>(json!(id))
        {
            match action {
                WebLinkAction::SystemBrowser => {
                    self.open_web_link(link, WebLinkTarget::SystemBrowser)
                }
                WebLinkAction::InAppBrowser => {
                    self.open_web_link(link, WebLinkTarget::InAppBrowser)
                }
                WebLinkAction::CopyLink => set_clipboard(link),
            }
            return;
        }
        crate::file_actions::choose(self.connection, self.files, target.link(link), id);
    }
}

/// Shared document semantics and metadata for NSH TextView.
fn message_parts(
    surroundings: Surroundings,
    source: impl Fn() -> Option<String> + 'static,
) -> Memo<Vec<Part>> {
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
    computed(move || {
        chips.with(|chips| {
            document.with(|(blocks, diagrams)| {
                search::indexed(parts(blocks.clone(), diagrams, &|file| {
                    Some(chips.chip(file))
                }))
            })
        })
    })
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
pub(crate) fn target(connection: Connection) -> Memo<String> {
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
#[cfg(desktop)]
pub(crate) fn action(surroundings: Surroundings, data: &Value) {
    if let Some(actions) = surroundings.external_actions
        && matches!(
            data["action"].as_str(),
            Some("quote" | "sideChat" | "explain")
        )
    {
        return actions.emit(data.clone());
    }
    let connection = surroundings.connection;
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
        _ => {}
    }
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

/// The selection's actions, as the Mac's text has them: the bar above the
/// selection, and a right click's menu with them beside Copy and Select
/// All, which act on the selection.
#[cfg(desktop)]
fn selection_actions(message: View, target: Memo<String>, surroundings: Surroundings) -> View {
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
                    action(
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
                    items
                        .push(MenuItem::new(tr("サイドチャットで質問")).on_select(act("sideChat")));
                }
                items.push(MenuItem::separator());
            }
            items.push(MenuItem::role(MenuRole::Copy).disabled(text.is_empty()));
            items.push(MenuItem::role(MenuRole::SelectAll));
            surroundings.selection.dismiss();
            drop(popup_menu_at(items, event.page_position));
        })
}

/// Application-specific inline controls inside the standard TextView.
mod inline {
    use crate::ui::colors;
    use nsh_core::{element::rgb, prelude::*};
    use serde_json::Value;

    /// What a paragraph's links do: `open` a link pressed, ⌘ (Ctrl here)
    /// held or not, and `choose` an item of a file chip's menu.
    #[derive(Clone, Copy)]
    pub(super) struct Links<O, C> {
        pub(super) open: O,
        pub(super) choose: C,
    }

    pub(super) fn highlighted_run(run: &Value, find: crate::find::Find) -> Text {
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
                let reference = element_ref();
                let key = find.key();
                let anchor = view().element_ref(&reference).w(px(0.0)).h(px(1.0));
                spawn(async move {
                    next_frame().await;
                    if let Some(key) = key {
                        find.reveal_anchor(reference, key);
                    }
                });
                span = span.child(anchor);
            }
            span = span.child(fragment);
        }
        span
    }

    /// A web link's explicit destinations share the native text actions.
    pub(super) fn web_span<O, C>(label: Text, link: String, menu: Value, links: Links<O, C>) -> Text
    where
        O: Fn(String, bool) + Copy + 'static,
        C: Fn(String, String) + Copy + 'static,
    {
        let (open, choose) = (links.open, links.choose);
        let pressed = link.clone();
        let span =
            label
                .text_color(colors::ACCENT)
                .underline()
                .on_press(move |event: &PressEvent| {
                    open(
                        pressed.clone(),
                        crate::file_actions::primary_modifier(event.modifiers, apple_keys()),
                    );
                });
        if !menu.is_array() {
            return span;
        }
        span.on_context_menu(move |_: &ContextMenuEvent| {
            let link = link.clone();
            crate::ui::popup(&menu, move |id: &str| choose(link.clone(), id.into()));
        })
    }

    /// A file chip as the desktop client's: its glyph and label in the file's accent, at
    /// medium weight, its full path and size over it, its menu on a secondary
    /// press; a press opens it as a link does.
    pub(super) fn chip_span<O, C>(
        label: Text,
        chip: &Value,
        link: String,
        links: Links<O, C>,
    ) -> Text
    where
        O: Fn(String, bool) + Copy + 'static,
        C: Fn(String, String) + Copy + 'static,
    {
        let color = super::run_color(chip).unwrap_or_else(|| colors::ACCENT.into());
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
            .on_press(move |event: &PressEvent| {
                open(
                    pressed.clone(),
                    crate::file_actions::primary_modifier(event.modifiers, apple_keys()),
                )
            })
            .on_context_menu(move |_: &ContextMenuEvent| {
                let link = link.clone();
                crate::ui::popup(&menu, move |id: &str| choose(link.clone(), id.into()));
            })
    }
}

/// A paragraph of the shared document; a run naming a file carries its chip.
fn paragraph(runs: Vec<MarkdownRun>, style: &MarkdownStyle, chip: Chip) -> Value {
    json!({
        "header": style.header, "marker": style.marker, "code": style.code,
        "quoted": style.quoted, "depth": style.list_depth,
        "alignment": match style.alignment { MarkdownAlignment::Left => "left", MarkdownAlignment::Center => "center", MarkdownAlignment::Right => "right" },
        "runs": runs.into_iter().map(|run| {
            let web_menu = run.link.as_deref()
                .filter(|link| web_link_target(link, WebLinkTarget::SystemBrowser, false).is_some())
                .map(|_| web_link_menu(crate::l10n::language()));
            let mut value = json!({
                "text": run.text, "strong": run.strong, "emphasis": run.emphasis,
                "strikethrough": run.strikethrough, "code": run.code, "link": run.link,
                "darkColor": run.dark_color, "lightColor": run.light_color,
            });
            if let Some(menu) = web_menu {
                value["webMenu"] = json!(menu);
            }
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
                result.push(Part::Rule {
                    depth: style.list_depth,
                    marker: style.marker,
                });
            }
            MarkdownBlock::Paragraph { runs, style } if style.code => {
                flush(&mut text, &mut result);
                result.push(code(runs, style, diagrams, chip));
            }
            MarkdownBlock::Paragraph { runs, mut style } => {
                let mut content = Vec::new();
                for run in runs {
                    if let Some(image) = run.image {
                        if !content.is_empty() {
                            text.push(paragraph(std::mem::take(&mut content), &style, chip));
                            style.marker = None;
                        }
                        flush(&mut text, &mut result);
                        result.push(Part::Image {
                            source: image,
                            depth: style.list_depth,
                            marker: style.marker.take(),
                        });
                    } else {
                        content.push(run);
                    }
                }
                if !content.is_empty() {
                    text.push(paragraph(content, &style, chip));
                }
            }
            MarkdownBlock::Visualization { path, style } => {
                flush(&mut text, &mut result);
                result.push(Part::Link {
                    url: path,
                    depth: style.list_depth,
                    marker: style.marker,
                });
            }
            MarkdownBlock::Table {
                source,
                columns,
                rows,
                style,
            } => {
                flush(&mut text, &mut result);
                result.push(Part::Table {
                    source,
                    depth: style.list_depth,
                    marker: style.marker,
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
    fn splitting_list_images_keeps_exactly_one_marker_on_the_first_part() {
        for (source, image_first) in [
            ("1. ![x](y.png) caption", true),
            ("1. before ![x](y.png) after", false),
        ] {
            let parts = parts(
                agent_core::presentation::markdown::markdown_blocks(source.into()),
                &[],
                &|_| None,
            );
            let markers: Vec<_> = parts
                .iter()
                .filter_map(|part| match part {
                    Part::Text(blocks) => blocks[0]["marker"].as_str(),
                    Part::Image { marker, .. } => marker.as_deref(),
                    _ => None,
                })
                .collect();
            assert_eq!(markers, ["1."]);
            assert_eq!(matches!(&parts[0], Part::Image { .. }), image_first);
            assert!(
                matches!(parts.last(), Some(Part::Text(blocks)) if blocks[0]["marker"].is_null())
            );
        }
    }

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
    fn annotated_colors_keep_both_appearances_and_use_the_available_variant() {
        for (run, expected) in [
            (
                json!({"lightColor":0x112233, "darkColor":0xaabbcc}),
                [0x112233ff, 0xaabbccff],
            ),
            (json!({"lightColor":0x112233}), [0x112233ff, 0x112233ff]),
            (json!({"darkColor":0xaabbcc}), [0xaabbccff, 0xaabbccff]),
        ] {
            let color = run_color(&run).unwrap();
            assert_eq!(nsh_core::Color::from(color.light).0, expected[0]);
            assert_eq!(nsh_core::Color::from(color.dark).0, expected[1]);
        }
        assert!(run_color(&json!({})).is_none());
        assert!(run_color(&json!({"lightColor":u64::MAX, "darkColor":"bad"})).is_none());
    }

    #[test]
    fn web_link_runs_carry_explicit_destinations_without_giving_file_links_web_actions() {
        let block = paragraph(
            [
                "https://example.com/path?q=1#part",
                "file:///tmp/readme.md",
                "mailto:user@example.com",
            ]
            .map(|link| MarkdownRun {
                text: link.into(),
                link: Some(link.into()),
                ..Default::default()
            })
            .into(),
            &MarkdownStyle::default(),
            &|_| None,
        );
        assert_eq!(
            block["runs"][0]["webMenu"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["systemBrowser", "inAppBrowser", "copyLink"]
        );
        assert!(block["runs"][1].get("webMenu").is_none());
        assert!(block["runs"][2].get("webMenu").is_none());
    }

    #[test]
    fn document_parts_preserve_media_code_rules_and_table_copy_sources_in_order() {
        let source = "前文 **強調**\n\n![画像](https://example.invalid/image.png)\n\n```rust title=main.rs\nlet 日本語 = 1;\n```\n\n---\n\n|名前|値|\n|:---|---:|\n|日本語|a,b|";
        let parts = parts(
            agent_core::presentation::markdown::markdown_blocks(source.into()),
            &[],
            &|_| None,
        );
        assert!(matches!(&parts[0], Part::Text(blocks) if blocks[0]["runs"][1]["strong"] == true));
        assert!(
            matches!(&parts[1], Part::Image { source, .. } if source == "https://example.invalid/image.png")
        );
        assert!(matches!(&parts[2], Part::Code { source, .. } if source == "let 日本語 = 1;"));
        assert!(matches!(&parts[3], Part::Rule { .. }));
        let Part::Table {
            source, csv, rows, ..
        } = &parts[4]
        else {
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
