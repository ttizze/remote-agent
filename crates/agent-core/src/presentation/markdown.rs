//! Native clients use the same GFM parser as GPUI's Markdown renderer.
use ::markdown::{
    ParseOptions,
    mdast::{AlignKind, Node},
};
use std::{borrow::Cow, collections::HashMap};
mod code;
#[cfg(feature = "diagrams")]
mod diagram;
#[cfg(feature = "diagrams")]
pub use diagram::{MarkdownDiagram, markdown_diagram};
mod links;
pub use links::{
    MarkdownFileKind, MarkdownFileReference, markdown_display_source, markdown_file_target,
};

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum MarkdownBlock {
    Visualization {
        path: String,
    },
    Paragraph {
        runs: Vec<MarkdownRun>,
        style: MarkdownStyle,
    },
    Table {
        source: String,
        columns: Vec<MarkdownAlignment>,
        rows: Vec<Vec<MarkdownCell>>,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum MarkdownAlignment {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct MarkdownStyle {
    pub alignment: MarkdownAlignment,
    pub header: Option<u8>,
    pub marker: Option<String>,
    pub code: bool,
    pub quoted: bool,
    pub list_depth: u32,
    pub language: Option<String>,
    pub filename: Option<String>,
    pub rule: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct MarkdownCell {
    pub runs: Vec<MarkdownRun>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct MarkdownRun {
    pub text: String,
    pub strong: bool,
    pub emphasis: bool,
    pub strikethrough: bool,
    pub code: bool,
    pub link: Option<String>,
    pub image: Option<String>,
    pub file: Option<MarkdownFileReference>,
    pub dark_color: Option<u32>,
    pub light_color: Option<u32>,
}

fn parse(source: &str) -> Node {
    // GFM has no syntax errors; only the disabled MDX extensions can fail.
    ::markdown::to_mdast(source, &ParseOptions::gfm()).expect("GFM is infallible")
}

fn definitions(node: &Node) -> HashMap<&str, &str> {
    fn collect<'a>(node: &'a Node, result: &mut HashMap<&'a str, &'a str>) {
        if let Node::Definition(def) = node {
            result.entry(&def.identifier).or_insert(&def.url);
        }
        for child in node.children().into_iter().flatten() {
            collect(child, result);
        }
    }
    let mut result = HashMap::new();
    collect(node, &mut result);
    result
}

fn image<'a>(node: &'a Node, definitions: &HashMap<&str, &'a str>) -> Option<(&'a str, &'a str)> {
    match node {
        Node::Image(image) => Some((&image.url, &image.alt)),
        Node::ImageReference(image) => definitions
            .get(image.identifier.as_str())
            .map(|url| (*url, image.alt.as_str())),
        _ => None,
    }
}

/// Separate images for Host-backed desktop loading without interpreting Markdown again in the client.
pub fn markdown_without_images(source: &str) -> (Cow<'_, str>, Vec<String>) {
    fn visit(
        node: &Node,
        definitions: &HashMap<&str, &str>,
        source: &str,
        previous: &mut usize,
        text: &mut String,
        images: &mut Vec<String>,
    ) {
        if let Some((url, _)) = image(node, definitions) {
            let position = node.position().expect("parsed image has a source position");
            text.push_str(&source[*previous..position.start.offset]);
            *previous = position.end.offset;
            images.push(url.to_owned());
        } else {
            for child in node.children().into_iter().flatten() {
                visit(child, definitions, source, previous, text, images);
            }
        }
    }
    let root = parse(source);
    let mut text = String::new();
    let mut images = Vec::new();
    let mut previous = 0;
    visit(
        &root,
        &definitions(&root),
        source,
        &mut previous,
        &mut text,
        &mut images,
    );
    if images.is_empty() {
        (Cow::Borrowed(source), images)
    } else {
        text.push_str(&source[previous..]);
        (Cow::Owned(text), images)
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn markdown_blocks(source: String) -> Vec<MarkdownBlock> {
    let root = parse(&source);
    let mut blocks = Vec::new();
    block(
        &root,
        &source,
        &definitions(&root),
        MarkdownStyle::default(),
        &mut blocks,
    );
    links::disambiguate(&mut blocks);
    blocks
}

/// CSV always quotes cells, including embedded delimiters, quotes and newlines.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn markdown_table_csv(rows: Vec<Vec<String>>) -> String {
    rows.iter()
        .map(|row| {
            row.iter()
                .map(|cell| format!("\"{}\"", cell.replace('"', "\"\"")))
                .collect::<Vec<_>>()
                .join(",")
        })
        .collect::<Vec<_>>()
        .join("\r\n")
}

/// Header labels keyed by source offset for renderers that retain the original Markdown AST.
pub fn markdown_code_headers(source: &str) -> HashMap<usize, String> {
    fn visit(node: &Node, headers: &mut HashMap<usize, String>) {
        if let Node::Code(code) = node
            && let Some(position) = node.position()
        {
            let label = code::fence_filename(code.meta.as_deref())
                .or_else(|| code::fence_filename(code.lang.as_deref()))
                .or_else(|| code.lang.clone())
                .unwrap_or_else(|| "CODE".into());
            headers.insert(position.start.offset, label);
        }
        for child in node.children().into_iter().flatten() {
            visit(child, headers);
        }
    }
    let mut headers = HashMap::new();
    visit(&parse(source), &mut headers);
    headers
}

/// Render plain code without allowing its contents to become Markdown links or directives.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn markdown_code_block(
    text: String,
    language: Option<String>,
    filename: Option<String>,
) -> MarkdownBlock {
    MarkdownBlock::Paragraph {
        runs: code::highlight(&text, language.as_deref()),
        style: MarkdownStyle {
            code: true,
            language,
            filename,
            ..Default::default()
        },
    }
}

fn inline(
    node: &Node,
    definitions: &HashMap<&str, &str>,
    mut style: MarkdownRun,
    runs: &mut Vec<MarkdownRun>,
) {
    if let Some((url, alt)) = image(node, definitions) {
        style.image = Some(url.to_owned());
        style.text = alt.to_owned();
        runs.push(style);
        return;
    }
    let destination = match node {
        Node::Link(link) => Some(link.url.as_str()),
        Node::LinkReference(link) => definitions.get(link.identifier.as_str()).copied(),
        _ => None,
    };
    if let Some(destination) = destination
        && let Some(file) = links::file_reference(destination, false)
    {
        let mut label = Vec::new();
        style.link = Some(destination.into());
        for child in node.children().into_iter().flatten() {
            inline(child, definitions, style.clone(), &mut label);
        }
        let text: String = label.iter().map(|run| run.text.as_str()).collect();
        if !links::is_file_label(&text, &file) {
            runs.extend(label);
            runs.push(MarkdownRun {
                text: " ".into(),
                ..Default::default()
            });
        }
        runs.push(MarkdownRun {
            text: file.label.clone(),
            link: Some(destination.into()),
            file: Some(file),
            ..Default::default()
        });
        return;
    }
    match node {
        Node::Strong(_) => style.strong = true,
        Node::Emphasis(_) => style.emphasis = true,
        Node::Delete(_) => style.strikethrough = true,
        Node::Link(link) => style.link = Some(link.url.clone()),
        Node::LinkReference(link) => {
            style.link = definitions
                .get(link.identifier.as_str())
                .map(|url| (*url).to_owned())
        }
        Node::InlineCode(code) => {
            style.code = true;
            style.text = code.value.clone();
            if style.link.is_none() {
                style.file = links::file_reference(&code.value, true);
                if let Some(file) = &style.file {
                    style.link = Some(code.value.clone());
                    style.text = file.label.clone();
                }
            }
        }
        Node::Text(text) => style.text = text.value.clone(),
        Node::Break(_) => style.text = "\n".into(),
        Node::Html(html) => style.text = html.value.clone(),
        Node::FootnoteReference(foot) => style.text = format!("[{}]", foot.identifier),
        _ => {}
    }
    if let Some(children) = node.children() {
        for child in children {
            inline(child, definitions, style.clone(), runs);
        }
    } else if !style.text.is_empty() {
        runs.push(style);
    }
}

fn block(
    node: &Node,
    source: &str,
    definitions: &HashMap<&str, &str>,
    mut style: MarkdownStyle,
    blocks: &mut Vec<MarkdownBlock>,
) {
    match node {
        Node::Definition(_) => return,
        Node::Table(table) => {
            let columns: Vec<_> = table
                .align
                .iter()
                .map(|alignment| match alignment {
                    AlignKind::Center => MarkdownAlignment::Center,
                    AlignKind::Right => MarkdownAlignment::Right,
                    _ => MarkdownAlignment::Left,
                })
                .collect();
            let rows = table
                .children
                .iter()
                .enumerate()
                .map(|(row_index, row)| {
                    let mut cells: Vec<_> = row
                        .children()
                        .into_iter()
                        .flatten()
                        .take(columns.len())
                        .map(|cell| {
                            let mut runs = Vec::new();
                            inline(
                                cell,
                                definitions,
                                MarkdownRun {
                                    strong: row_index == 0,
                                    ..Default::default()
                                },
                                &mut runs,
                            );
                            MarkdownCell { runs }
                        })
                        .collect();
                    cells.resize_with(columns.len(), MarkdownCell::default);
                    cells
                })
                .collect();
            let position = node.position().expect("parsed table has a source position");
            blocks.push(MarkdownBlock::Table {
                source: source[position.start.offset..position.end.offset].into(),
                columns,
                rows,
            });
            return;
        }
        Node::List(list) => {
            let depth = style.list_depth;
            for (index, child) in list.children.iter().enumerate() {
                let mut item_style = style.clone();
                item_style.list_depth = depth + 1;
                item_style.marker = Some(
                    if let Node::ListItem(item) = child
                        && let Some(checked) = item.checked
                    {
                        if checked { "☑" } else { "☐" }.into()
                    } else if list.ordered {
                        format!("{}.", u64::from(list.start.unwrap_or(1)) + index as u64)
                    } else {
                        ["•", "◦", "▪"][depth as usize % 3].into()
                    },
                );
                block(child, source, definitions, item_style, blocks);
            }
            return;
        }
        Node::Blockquote(_) => style.quoted = true,
        Node::Code(code) => {
            style.code = true;
            style.language = code.lang.clone();
            style.filename = code::fence_filename(code.meta.as_deref())
                .or_else(|| code::fence_filename(code.lang.as_deref()));
            blocks.push(MarkdownBlock::Paragraph {
                runs: code::highlight(&code.value, code.lang.as_deref()),
                style,
            });
            return;
        }
        Node::Heading(heading) => style.header = Some(heading.depth),
        Node::ThematicBreak(_) => {
            style.rule = true;
            blocks.push(MarkdownBlock::Paragraph {
                runs: vec![MarkdownRun {
                    text: "―".into(),
                    ..Default::default()
                }],
                style,
            });
            return;
        }
        _ => {}
    }
    // Read opaque JSON before Markdown unescapes Windows path separators.
    if matches!(node, Node::Paragraph(_))
        && let Some(position) = node.position()
        && let Some(path) =
            visualization_reference(&source[position.start.offset..position.end.offset])
    {
        blocks.push(MarkdownBlock::Visualization { path });
        return;
    }
    if matches!(node, Node::Paragraph(_) | Node::Heading(_) | Node::Html(_)) {
        let mut runs = Vec::new();
        inline(node, definitions, MarkdownRun::default(), &mut runs);
        blocks.push(MarkdownBlock::Paragraph { runs, style });
    } else {
        for child in node.children().into_iter().flatten() {
            block(child, source, definitions, style.clone(), blocks);
            // A list marker belongs to the first paragraph, not every continuation.
            if matches!(node, Node::ListItem(_)) {
                style.marker = None;
            }
        }
    }
}

fn visualization_reference(text: &str) -> Option<String> {
    let json = text.trim().strip_prefix("visualize")?.strip_suffix("")?;
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let path = value.get("path")?.as_str()?;
    (!path.is_empty()).then(|| path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn table_copy_preserves_original_markdown_and_quotes_csv() {
        let original = "| **太字** *斜体* ~~削除~~ | ``a`b`` |\n| :--- | ---: |\n| [資料](https://example.com) | a\\|b |";
        let blocks = markdown_blocks(format!("before\n\n{original}\n\nafter"));
        let MarkdownBlock::Table { source, .. } = &blocks[1] else {
            panic!()
        };
        assert_eq!(source, original);
        assert_eq!(
            markdown_table_csv(vec![vec!["a,b".into(), "a\"b\nc".into()]]),
            "\"a,b\",\"a\"\"b\nc\""
        );
    }

    #[test]
    fn fenced_metadata_nested_lists_and_rules_retain_structure() {
        let blocks = markdown_blocks("- outer\n  - inner\n\n    continuation\n\n```rust title=\"src/hello world.rs\"\nfn main() {}\n```\n\n---".into());
        let styles: Vec<_> = blocks
            .iter()
            .filter_map(|block| match block {
                MarkdownBlock::Paragraph { style, .. } => Some(style),
                _ => None,
            })
            .collect();
        assert_eq!(styles[0].list_depth, 1);
        assert_eq!(styles[1].list_depth, 2);
        assert_eq!(styles[2].list_depth, 2);
        assert!(styles[2].marker.is_none());
        assert_eq!(styles[3].language.as_deref(), Some("rust"));
        assert_eq!(styles[3].filename.as_deref(), Some("src/hello world.rs"));
        assert!(styles[4].rule);
    }

    #[test]
    fn visualize_reference_is_a_distinct_block() {
        let blocks = markdown_blocks("Before\n\nvisualize{\"path\":\"/fixture/icon-options.html\",\"mode\":\"wide\"}\n\nAfter".into());
        assert_eq!(blocks.len(), 3);
        assert_eq!(
            blocks[1],
            MarkdownBlock::Visualization {
                path: "/fixture/icon-options.html".into()
            }
        );
        for source in [
            "`visualize{\"path\":\"x.html\"}`",
            "```text\nvisualize{\"path\":\"x.html\"}\n```",
            "visualize{broken}",
            "visualize{\"path\":\"\"}",
            "visualize{\"path\":null}",
            "visualize{\"path\":\"x.html\"}",
        ] {
            assert!(matches!(
                markdown_blocks(source.into())[0],
                MarkdownBlock::Paragraph { .. }
            ));
        }
    }

    proptest::proptest! {
        #[test]
        fn visualization_paths_preserve_json_escapes(name in "[^\\x00\\r\\n]{1,32}") {
            let path = format!("C:\\fixture\\{name}.html");
            let source = format!(
                "Before\n\nvisualize{}\n\nAfter",
                serde_json::json!({"path": path}),
            );
            let blocks = markdown_blocks(source);
            proptest::prop_assert_eq!(blocks.len(), 3);
            proptest::prop_assert_eq!(
                &blocks[1],
                &MarkdownBlock::Visualization { path },
            );
        }
    }

    const TABLE: &str = include_str!("../../tests/fixtures/markdown/table.md");

    fn cell_text(cell: &MarkdownCell) -> String {
        cell.runs.iter().map(|run| run.text.as_str()).collect()
    }

    fn paragraph_text(block: &MarkdownBlock) -> String {
        let MarkdownBlock::Paragraph { runs, .. } = block else {
            panic!("expected paragraph")
        };
        runs.iter().map(|run| run.text.as_str()).collect()
    }

    #[test]
    fn markdown_table_preserves_reported_cells_and_surrounding_prose() {
        let blocks = markdown_blocks(format!("Before\n\n{TABLE}\nAfter"));
        assert_eq!(blocks.len(), 3);
        assert_eq!(paragraph_text(&blocks[0]), "Before");
        assert_eq!(paragraph_text(&blocks[2]), "After");
        let MarkdownBlock::Table { columns, rows, .. } = &blocks[1] else {
            panic!("table flattened into prose")
        };
        assert_eq!(columns, &[MarkdownAlignment::Left; 3]);
        insta::assert_json_snapshot!(
            rows.iter()
                .map(|row| row.iter().map(cell_text).collect::<Vec<_>>())
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn markdown_table_preserves_empty_cells_alignment_and_inline_semantics() {
        let source = "| Left | Center | Right |\n|:---|:---:|---:|\n| **bold** and [link][ref] | | `code` |\n| | | |\n| last | escaped \\| pipe | ~~gone~~ |\n| | | |\n\n[ref]: https://example.com\n";
        let blocks = markdown_blocks(source.into());
        let MarkdownBlock::Table { columns, rows, .. } = &blocks[0] else {
            panic!("expected table")
        };
        assert_eq!(
            columns,
            &[
                MarkdownAlignment::Left,
                MarkdownAlignment::Center,
                MarkdownAlignment::Right
            ]
        );
        insta::assert_json_snapshot!(
            rows.iter()
                .map(|row| row.iter().map(cell_text).collect::<Vec<_>>())
                .collect::<Vec<_>>(),
        );
        assert!(rows[1][0].runs[0].strong);
        assert!(
            rows[0]
                .iter()
                .flat_map(|cell| &cell.runs)
                .all(|run| run.strong)
        );
        assert!(!rows[3][0].runs[0].strong);
        assert_eq!(
            rows[1][0].runs.last().unwrap().link.as_deref(),
            Some("https://example.com")
        );
        assert!(rows[1][2].runs[0].code);
        assert!(rows[3][2].runs[0].strikethrough);
    }

    #[test]
    fn markdown_table_streaming_and_non_table_source_remain_valid() {
        for (end, _) in TABLE.char_indices() {
            let blocks = markdown_blocks(TABLE[..end].into());
            for block in blocks {
                if let MarkdownBlock::Table { columns, rows, .. } = block {
                    assert!(rows.iter().all(|row| row.len() == columns.len()));
                }
            }
        }
        for (source, expected) in [
            ("No table | here".to_owned(), "No table | here".to_owned()),
            (
                format!("```text\n{TABLE}```"),
                TABLE.trim_end_matches(['\r', '\n']).to_owned(),
            ),
            (
                "| incomplete |\n| text".into(),
                "| incomplete |\n| text".into(),
            ),
        ] {
            let blocks = markdown_blocks(source);
            assert_eq!(blocks.len(), 1);
            assert_eq!(paragraph_text(&blocks[0]), expected);
        }
        // A single hyphen is already a valid GFM delimiter while streaming.
        assert!(
            matches!(markdown_blocks("| incomplete |\n| -".into()).as_slice(),
            [MarkdownBlock::Table { rows, .. }] if rows.len() == 1)
        );
        assert_eq!(markdown_blocks(format!("{TABLE}\n{TABLE}")).len(), 2);
    }
    #[test]
    fn markdown_document_preserves_shared_prose_semantics() {
        let source = include_str!("../../tests/fixtures/markdown/document.md");
        let blocks = markdown_blocks(source.into());
        let paragraphs: Vec<_> = blocks
            .iter()
            .map(|block| {
                let MarkdownBlock::Paragraph { runs, style } = block else {
                    panic!("unexpected table")
                };
                (runs, style)
            })
            .collect();
        assert_eq!(paragraphs.len(), 10);
        assert_eq!(paragraphs[0].1.header, Some(1));
        assert_eq!(paragraph_text(&blocks[1]), "前の 太字と 強調、取消、参照。");
        assert!(
            paragraphs[1]
                .0
                .iter()
                .any(|run| run.text == "強調" && run.strong && run.emphasis)
        );
        assert!(
            paragraphs[1]
                .0
                .iter()
                .any(|run| run.text == "取消" && run.strikethrough)
        );
        assert!(paragraphs[2].1.quoted);
        assert_eq!(paragraphs[3].1.marker.as_deref(), Some("3."));
        assert_eq!(paragraphs[4].1.marker.as_deref(), Some("4."));
        assert_eq!(paragraphs[5].1.marker.as_deref(), Some("☑"));
        assert_eq!(paragraphs[6].1.marker.as_deref(), Some("☐"));
        assert!(paragraphs[7].1.code);
        assert_eq!(paragraphs[7].0[0].image, None);
        assert_eq!(
            paragraphs[8].0[0].image.as_deref(),
            Some("images/example.png")
        );
        for index in [1, 9] {
            assert!(
                paragraphs[index]
                    .0
                    .iter()
                    .any(|run| run.link.as_deref() == Some("https://example.com/reference"))
            );
        }
    }

    #[test]
    fn desktop_image_extraction_preserves_source_and_reference_context() {
        let source = include_str!("../../tests/fixtures/markdown/document.md");
        let (rendered, images) = markdown_without_images(source);
        assert_eq!(images, ["images/example.png"]);
        assert_eq!(rendered, source.replace("![画像][picture]", ""));
        let source =
            "![first](a.png) **![second](b.png)** [![third][image]](target)\n\n[image]: c.png";
        let (rendered, images) = markdown_without_images(source);
        assert_eq!(images, ["a.png", "b.png", "c.png"]);
        assert_eq!(rendered, " **** [](target)\n\n[image]: c.png");
        let (rendered, images) = markdown_without_images(TABLE);
        assert_eq!(rendered, TABLE);
        assert!(images.is_empty());
    }
}
