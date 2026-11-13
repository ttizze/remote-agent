//! Shared transcript parts, link metadata and actions for native conversation views.
use crate::Connection;
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
pub(crate) use search::documents as search_documents;

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) enum Part {
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
pub(crate) struct Diagram {
    svg: String,
    png: String,
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
pub(crate) fn diagrams(source: &str) -> Vec<(String, Diagram)> {
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
                                png: diagram_png(&diagram.svg).unwrap_or_default(),
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

fn diagram_png(svg: &str) -> Option<String> {
    use base64::Engine;
    use resvg::{tiny_skia, usvg};
    let tree = usvg::Tree::from_str(svg, &usvg::Options::default()).ok()?;
    let scale = (1600.0 / tree.size().width().max(tree.size().height())).min(2.0);
    let size = tree.size().to_int_size().scale_by(scale)?;
    let mut pixels = tiny_skia::Pixmap::new(size.width(), size.height())?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixels.as_mut(),
    );
    Some(base64::engine::general_purpose::STANDARD.encode(pixels.encode_png().ok()?))
}

pub(crate) fn highlight(
    parts: &mut [Part],
    query: &str,
    active: Option<(usize, agent_core::presentation::conversation::TextMatch)>,
) {
    for part in parts {
        match part {
            Part::Text(blocks) | Part::Code { blocks, .. } => {
                let index = search::document(blocks);
                *blocks = search::decorate(
                    std::mem::take(blocks),
                    query,
                    active
                        .filter(|(document, _)| *document == index)
                        .map(|(_, range)| range),
                );
            }
            Part::Table { rows, .. } => {
                for cell in rows.iter_mut().flatten() {
                    highlight(cell, query, active);
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn indexed(parts: Vec<Part>) -> Vec<Part> {
    search::indexed(parts)
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
    pub external_actions: Option<crate::native::ExternalActions>,
    pub find: crate::find::Find,
}

impl Surroundings {
    /// The surroundings of the list being made, with a selection bar of its own.
    pub(crate) fn current() -> Self {
        Self {
            connection: use_context::<Connection>().unwrap(),
            files: use_context::<crate::files::Browser>().unwrap(),
            preview: use_context::<crate::media::Preview>().unwrap(),
            workspace: use_context::<crate::workbench::Workbench>().unwrap(),
            external_actions: use_context::<crate::native::ExternalActions>(),
            find: use_context::<crate::find::Find>().unwrap(),
        }
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

/// Host paths for the document's file metadata, including files in tables.
pub(crate) fn file_paths(blocks: &[MarkdownBlock]) -> Vec<String> {
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
        Some("link") => surroundings.open_link(
            text.into(),
            target,
            data["command"].as_bool().unwrap_or(false),
        ),
        Some("file" | "webLink") => {
            surroundings.choose_link(text.into(), target, data["id"].as_str().unwrap_or_default())
        }
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
pub(crate) fn parts(
    blocks: Vec<MarkdownBlock>,
    diagrams: &[(String, Diagram)],
    chip: Chip,
) -> Vec<Part> {
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
