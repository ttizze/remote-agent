use super::super::Part;
#[cfg(test)]
use super::super::{MarkdownBlock, MarkdownRun, MarkdownStyle};
use agent_core::presentation::conversation::TextMatch;
use nsh_base::text_view::{Block, CodeBlock, Document, Inline};
use nsh_base::text_view::{ColumnAlign, CustomNode, ListItem, Table, TableCell};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};

/// A single TextView document, with application semantics carried by custom
/// inline nodes rather than a separate view tree for every paragraph.
pub(super) fn presentation(
    parts: Vec<Part>,
    query: &str,
    active: Option<(usize, TextMatch)>,
    navigation: Option<&str>,
    wrapping: &HashMap<String, bool>,
) -> Document {
    let mut output = Vec::new();
    for part in parts {
        let (depth, marker) = match &part {
            Part::Code { blocks, .. } => (
                blocks.first().map(list_depth).unwrap_or_default(),
                blocks
                    .first()
                    .and_then(|block| block["marker"].as_str())
                    .map(str::to_owned),
            ),
            Part::Image { depth, marker, .. }
            | Part::Table { depth, marker, .. }
            | Part::Link { depth, marker, .. }
            | Part::Rule { depth, marker } => (u64::from(*depth), marker.clone()),
            Part::Text(_) => (0, None),
        };
        let block = match part {
            Part::Text(blocks) => {
                output.extend(decorate(blocks, query, active, navigation).into_iter().map(
                    |block| Row {
                        depth: list_depth(&block),
                        marker: block["marker"].as_str().map(str::to_owned),
                        block: value_paragraph(&block),
                    },
                ));
                continue;
            }
            Part::Code {
                diagram: Some(_),
                ref source,
                ref blocks,
                ..
            } => Block::Custom(
                CustomNode::new("bex:diagram")
                    .with_attribute(
                        "document",
                        super::super::search::document(blocks).to_string(),
                    )
                    .with_text(source.clone())
                    .with_data(json!(part)),
            ),
            Part::Code {
                label,
                source,
                blocks,
                ..
            } => {
                let id = super::super::search::document(&blocks).to_string();
                let mut code = CodeBlock::new(Some(label), source);
                code.wrap = wrapping.get(&id).copied().unwrap_or(true);
                code.id = Some(id);
                for (index, block) in decorate(blocks, query, active, navigation)
                    .iter()
                    .enumerate()
                {
                    if index != 0 {
                        code.content.push(Inline::text("\n"));
                    }
                    code.content.extend(value_content(block));
                }
                Block::CodeBlock(code)
            }
            Part::Table { rows, .. } => {
                let mut table = Table::default();
                table.align = rows[0]
                    .iter()
                    .map(|cell| {
                        let alignment = cell.iter().find_map(|part| match part {
                            Part::Text(blocks) => {
                                blocks.first().and_then(|block| block["alignment"].as_str())
                            }
                            _ => None,
                        });
                        match alignment {
                            Some("center") => ColumnAlign::Center,
                            Some("right") => ColumnAlign::Right,
                            _ => ColumnAlign::Left,
                        }
                    })
                    .collect();
                let mut rows = rows.into_iter().map(|row| {
                    row.into_iter()
                        .map(|cell| {
                            let document = presentation(cell, query, active, navigation, wrapping);
                            let mut content = Vec::new();
                            for block in document.blocks {
                                match block {
                                    Block::Paragraph { content: runs }
                                    | Block::Heading { content: runs, .. } => content.extend(runs),
                                    Block::Custom(node) => content.push(Inline::Custom(node)),
                                    other => content.push(Inline::text(other.plain_text())),
                                }
                            }
                            TableCell::new(content)
                        })
                        .collect::<Vec<_>>()
                });
                table.head = rows.next().unwrap_or_default();
                table.rows = rows.collect();
                Block::Table(table)
            }
            Part::Image { source, .. } => {
                Block::Custom(CustomNode::new("bex:image").with_data(json!(source)))
            }
            Part::Link { url, .. } => Block::paragraph(vec![Inline::Link {
                content: vec![Inline::text(url.clone())],
                url,
                title: None,
            }]),
            Part::Rule { .. } => Block::Rule,
        };
        output.push(Row {
            depth,
            marker,
            block,
        });
    }
    Document::new(hierarchy(output))
}

/// The common parse keeps list position on every block, including media and
/// tables. Build lists after converting all parts so a continuation stays in
/// its item across a code block, image, diagram or table.
struct Row {
    depth: u64,
    marker: Option<String>,
    block: Block,
}

fn hierarchy(rows: Vec<Row>) -> Vec<Block> {
    let mut rows: VecDeque<_> = rows.into();
    let mut output = Vec::new();
    while let Some(row) = rows.front() {
        if row.depth > 0 && row.marker.is_some() {
            output.push(list(&mut rows));
        } else {
            output.push(rows.pop_front().unwrap().block);
        }
    }
    output
}

fn list_depth(block: &Value) -> u64 {
    block["depth"].as_u64().unwrap_or_default()
}

fn list(rows: &mut VecDeque<Row>) -> Block {
    let first = rows.front().unwrap();
    let level = first.depth;
    let marker = first.marker.as_deref().unwrap();
    let start = marker.trim_end_matches('.').parse::<u32>().ok();
    let ordered = start.is_some();
    let mut items: Vec<ListItem> = Vec::new();
    while let Some(row) = rows.front().filter(|row| row.depth >= level) {
        if row.depth > level && row.marker.is_some() {
            let nested = list(rows);
            items.last_mut().unwrap().children.push(nested);
            continue;
        }
        let is_ordered = row
            .marker
            .as_deref()
            .is_some_and(|marker| marker.trim_end_matches('.').parse::<u32>().is_ok());
        if row.depth == level && row.marker.is_some() && !items.is_empty() && is_ordered != ordered
        {
            break;
        }
        let row = rows.pop_front().unwrap();
        if row.marker.is_some() && row.depth == level {
            let mut item = ListItem::default();
            item.checked = match row.marker.as_deref() {
                Some("☑") => Some(true),
                Some("☐") => Some(false),
                _ => None,
            };
            items.push(item);
        }
        items.last_mut().unwrap().children.push(row.block);
    }
    Block::List {
        ordered,
        start: start.unwrap_or(1),
        loose: false,
        items,
    }
}

fn decorate(
    blocks: Vec<Value>,
    query: &str,
    active: Option<(usize, TextMatch)>,
    navigation: Option<&str>,
) -> Vec<Value> {
    let document = super::super::search::document(&blocks);
    let selected = active
        .filter(|(index, _)| *index == document)
        .map(|(_, range)| range);
    let mut blocks = super::super::search::decorate(blocks, query, selected);
    for block in &mut blocks {
        if super::super::search::reveals(block) {
            for run in block["runs"].as_array_mut().into_iter().flatten() {
                run["findKey"] = json!(navigation);
            }
        }
    }
    blocks
}

fn value_paragraph(block: &Value) -> Block {
    let content = value_content(block);
    let paragraph = match block["header"].as_u64().and_then(|n| u8::try_from(n).ok()) {
        Some(level) => Block::Heading {
            level,
            content,
            anchor: None,
        },
        None => Block::paragraph(content),
    };
    if block["quoted"] == true {
        Block::Blockquote {
            children: vec![paragraph],
            alert: None,
        }
    } else {
        paragraph
    }
}

fn value_content(block: &Value) -> Vec<Inline> {
    let mut content = Vec::new();
    // TextView owns list markers. A marker-only hit still needs an anchor at
    // the item start, without adding a second marker to selection or copying.
    if block["markerHighlights"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|hit| hit["reveal"] == true)
    {
        let navigation = block["runs"]
            .as_array()
            .and_then(|runs| runs.first())
            .map(|run| run["findKey"].clone());
        content.push(value_inline(&json!({
            "text":"", "findKey":navigation,
            "highlights":[{"start":0,"end":0,"active":true,"reveal":true}]
        })));
    }
    content.extend(
        block["runs"]
            .as_array()
            .into_iter()
            .flatten()
            .map(value_inline),
    );
    content
}

fn value_inline(run: &Value) -> Inline {
    let label = run["text"].as_str().unwrap_or_default().to_owned();
    let file = run.get("file").is_some();
    let decorated =
        run["highlights"].as_array().is_some_and(|r| !r.is_empty()) || run["darkColor"].is_u64();
    let mut node = if file {
        Inline::Custom(
            CustomNode::new("bex:file")
                .with_text(label.clone())
                .with_data(run.clone()),
        )
    } else if decorated {
        Inline::Custom(
            CustomNode::new("bex:run")
                .with_text(label.clone())
                .with_data(run.clone()),
        )
    } else {
        Inline::text(label.clone())
    };
    if run["code"] == true && !file {
        // Keep highlights and anchors inside inline-code styling.
        node = if decorated {
            Inline::Custom(
                CustomNode::new("bex:code-run")
                    .with_text(label.clone())
                    .with_data(run.clone()),
            )
        } else {
            Inline::Code {
                code: label.clone(),
            }
        };
    }
    if run["strong"] == true {
        node = Inline::Strong {
            content: vec![node],
        };
    }
    if run["emphasis"] == true {
        node = Inline::Emphasis {
            content: vec![node],
        };
    }
    if run["strikethrough"] == true {
        node = Inline::Strikethrough {
            content: vec![node],
        };
    }
    if let Some(url) = run["link"].as_str().filter(|_| !file) {
        node = Inline::Link {
            url: url.into(),
            title: None,
            content: vec![node],
        };
    }
    node
}

#[cfg(test)]
fn document(blocks: Vec<MarkdownBlock>) -> Document {
    presentation(
        super::super::parts(blocks, &[], &|_| None),
        "",
        None,
        None,
        &HashMap::new(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_code_keeps_literal_characters_and_markdown_tables_stay_tables() {
        let mut blocks = vec![MarkdownBlock::Paragraph {
            runs: vec![MarkdownRun {
                text: "a < b && **literal**\n日本語".into(),
                ..Default::default()
            }],
            style: MarkdownStyle {
                code: true,
                language: Some("rust".into()),
                ..Default::default()
            },
        }];
        blocks.extend(agent_core::presentation::markdown::markdown_blocks(
            "| A | B |\n|---|---|\n|日本語|`x`|".into(),
        ));
        let doc = document(blocks);
        assert!(
            matches!(&doc.blocks[0], Block::CodeBlock(code) if code.code == "a < b && **literal**\n日本語" && code.language.as_deref() == Some("rust"))
        );
        assert!(
            matches!(&doc.blocks[1], Block::Table(table) if table.head.len() == 2 && table.rows.len() == 1)
        );
        assert!(doc.plain_text().contains("日本語\tx"));
    }

    #[test]
    fn linked_emphasized_inline_code_keeps_content_without_reparsing() {
        let node = value_inline(
            &json!({"text":"*literal*", "code":true, "strong":true, "link":"hello.txt"}),
        );
        let Inline::Link { url, content, .. } = node else {
            panic!("missing link")
        };
        assert_eq!(url, "hello.txt");
        assert!(
            matches!(&content[0], Inline::Strong { content } if matches!(&content[0], Inline::Code { code } if code == "*literal*"))
        );
    }
    #[test]
    fn code_controls_and_search_keep_the_document_identity_and_literal_text() {
        let source = "prefix 日本語 suffix";
        let part = Part::Code {
            label: "text".into(),
            source: source.into(),
            diagram: None,
            blocks: vec![json!({"findDocument":0,"runs":[{"text":source}]})],
        };
        let wrapping = HashMap::from([("0".into(), false)]);
        let make = |navigation| {
            presentation(
                vec![part.clone()],
                "日本",
                Some((0, TextMatch { start: 7, end: 13 })),
                Some(navigation),
                &wrapping,
            )
        };
        let doc = make("first");
        let Block::CodeBlock(code) = &doc.blocks[0] else {
            panic!("code block")
        };
        assert_eq!(code.id.as_deref(), Some("0"));
        assert!(!code.wrap);
        assert_eq!(code.code, source);
        assert_eq!(
            code.content
                .iter()
                .map(Inline::plain_text)
                .collect::<String>(),
            source
        );
        let Inline::Custom(run) = &code.content[0] else {
            panic!("search annotation")
        };
        assert_eq!(run.data["findKey"], "first");
        assert_ne!(
            doc,
            make("next"),
            "navigation must re-render a sole active match"
        );
    }

    #[test]
    fn diagram_controls_keep_their_message_document_identity() {
        let part = Part::Code {
            label: "mermaid".into(),
            source: "flowchart LR".into(),
            blocks: vec![json!({"findDocument":5,"runs":[{"text":"flowchart LR"}]})],
            diagram: Some(crate::rich_text::Diagram {
                svg: "<svg/>".into(),
                width: 1.0,
                height: 1.0,
            }),
        };
        let doc = presentation(vec![part], "", None, None, &HashMap::new());
        let Block::Custom(node) = &doc.blocks[0] else {
            panic!("diagram")
        };
        assert_eq!(
            node.attributes.get("document").map(String::as_str),
            Some("5")
        );
        assert_eq!(node.text, "flowchart LR");
    }

    #[test]
    fn lists_keep_nested_items_tasks_and_continuations() {
        let source = "3. Parent\n   - Child\n   - [x] Done\n\n     Continued\n4. Next";
        let blocks = agent_core::presentation::markdown::workspace_markdown_blocks(
            source.into(),
            "/project".into(),
            None,
        );
        let doc = document(blocks);
        let Block::List {
            ordered,
            start,
            items,
            ..
        } = &doc.blocks[0]
        else {
            panic!("missing list")
        };
        assert!(*ordered);
        assert_eq!(*start, 3);
        assert_eq!(items.len(), 2);
        let Block::List { items: nested, .. } = &items[0].children[1] else {
            panic!("missing nested list")
        };
        assert_eq!(nested.len(), 2);
        assert_eq!(nested[1].checked, Some(true));
        assert!(doc.plain_text().contains("Continued"));
    }

    #[test]
    fn marker_only_find_hit_reveals_the_item_without_duplicating_its_text() {
        let parts = super::super::super::search::indexed(super::super::super::parts(
            agent_core::presentation::markdown::markdown_blocks("2. Item".into()),
            &[],
            &|_| None,
        ));
        let doc = presentation(
            parts,
            "2.",
            Some((0, TextMatch { start: 0, end: 2 })),
            Some("marker-hit"),
            &HashMap::new(),
        );
        let Block::List { items, .. } = &doc.blocks[0] else {
            panic!("list")
        };
        let Block::Paragraph { content } = &items[0].children[0] else {
            panic!("paragraph")
        };
        let Inline::Custom(anchor) = &content[0] else {
            panic!("marker anchor")
        };
        assert!(anchor.text.is_empty());
        assert_eq!(anchor.data["findKey"], "marker-hit");
        assert_eq!(anchor.data["highlights"][0]["reveal"], true);
        assert_eq!(items[0].children[0].plain_text(), "Item");
    }

    #[test]
    fn list_items_keep_code_images_tables_and_the_following_paragraph_together() {
        let source = "1. Parent\n\n   ```sh\n   echo hi\n   ```\n\n   ![picture](picture.png)\n\n   | A |\n   |---|\n   | B |\n\n   Continued\n2. Next";
        let blocks = agent_core::presentation::markdown::workspace_markdown_blocks(
            source.into(),
            "/project".into(),
            None,
        );
        let doc = document(blocks);
        let [
            Block::List {
                ordered: true,
                items,
                ..
            },
        ] = doc.blocks.as_slice()
        else {
            panic!("one ordered list, got {:?}", doc.blocks)
        };
        assert_eq!(items.len(), 2);
        let children = &items[0].children;
        assert!(matches!(&children[1], Block::CodeBlock(code) if code.code == "echo hi"));
        assert!(matches!(&children[2], Block::Custom(node) if node.name == "bex:image"));
        assert!(matches!(&children[3], Block::Table(_)));
        assert_eq!(children[4].plain_text(), "Continued");
        assert_eq!(items[1].children[0].plain_text(), "Next");
    }

    #[test]
    fn table_alignment_and_file_custom_nodes_survive_conversion() {
        let doc = presentation(
            vec![Part::Table {
                source: String::new(),
                csv: String::new(),
                depth: 0,
                marker: None,
                rows: vec![vec![vec![Part::Text(vec![
                    json!({"alignment":"right", "runs":[{"text":"hello.txt", "file":{"path":"hello.txt"}}]}),
                ])]]],
            }],
            "",
            None,
            None,
            &HashMap::new(),
        );
        let Block::Table(table) = &doc.blocks[0] else {
            panic!("missing table")
        };
        assert_eq!(table.align, [ColumnAlign::Right]);
        assert_eq!(table.head[0].content[0].plain_text(), "hello.txt");
    }
}
