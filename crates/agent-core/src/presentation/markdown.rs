//! Every client renders Markdown from this crate's single GFM parse.
use ::markdown::{
    ParseOptions,
    mdast::{AlignKind, Node},
};
use std::{borrow::Cow, collections::HashMap};
mod code;
#[cfg(feature = "diagrams")]
mod diagram;
#[cfg(feature = "diagrams")]
pub use diagram::{MarkdownDiagram, markdown_diagram, markdown_mermaid_fences};
mod links;
pub use links::{
    MarkdownFileKind, MarkdownFileReference, MarkdownFileSurface, WORKSPACE_BASENAME_LOOKUP_LIMIT,
    file_basename, file_host_path, file_path_for_clipboard, file_position_target,
    file_workspace_relative_path, markdown_file_kind, markdown_file_target,
    needs_workspace_basename_lookup, pick_stripped_workspace_match, pick_workspace_basename_match,
    strip_repeated_workspace_prefix,
};

/// Every block retains its containing list/quote style so clients can keep
/// code, media, tables and continuations in one standard document hierarchy.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum MarkdownBlock {
    Visualization {
        path: String,
        style: MarkdownStyle,
    },
    Paragraph {
        runs: Vec<MarkdownRun>,
        style: MarkdownStyle,
    },
    Table {
        source: String,
        columns: Vec<MarkdownAlignment>,
        rows: Vec<Vec<MarkdownCell>>,
        style: MarkdownStyle,
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

fn is_disclosure_tag(source: &str) -> bool {
    let Some(name) = source.strip_prefix('<') else {
        return false;
    };
    let name = name.strip_prefix('/').unwrap_or(name);
    let name = name
        .split(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>')
        .next()
        .unwrap_or_default();
    name.eq_ignore_ascii_case("details") || name.eq_ignore_ascii_case("summary")
}

/// Show disclosure captions and contents on every client without exposing HTML wrappers.
/// Use parsed HTML positions so escaped text and code examples keep their literal tags.
fn without_disclosure_tags(source: &str) -> Cow<'_, str> {
    if !source
        .match_indices('<')
        .any(|(offset, _)| is_disclosure_tag(&source[offset..]))
    {
        return Cow::Borrowed(source);
    }
    fn tags(node: &Node, base: usize, ranges: &mut Vec<std::ops::Range<usize>>) {
        if let Node::Html(html) = node
            && is_disclosure_tag(&html.value)
        {
            let position = node.position().expect("parsed HTML has a source position");
            ranges.push(base + position.start.offset..base + position.end.offset);
        }
        for child in node.children().into_iter().flatten() {
            tags(child, base, ranges);
        }
    }
    fn visit(node: &Node, source: &str, ranges: &mut Vec<std::ops::Range<usize>>) {
        if let Node::Html(html) = node {
            if is_disclosure_tag(html.value.trim_start()) {
                let position = node.position().expect("parsed HTML has a source position");
                // A flow HTML node can swallow the entire disclosure, including images.
                // Read its tags as inline HTML while retaining Markdown code boundaries.
                let mut options = ParseOptions::gfm();
                options.constructs.html_flow = false;
                let fragment = ::markdown::to_mdast(
                    &source[position.start.offset..position.end.offset],
                    &options,
                )
                .expect("GFM is infallible");
                tags(&fragment, position.start.offset, ranges);
            }
            return;
        }
        for child in node.children().into_iter().flatten() {
            visit(child, source, ranges);
        }
    }
    let mut ranges = Vec::new();
    visit(&parse(source), source, &mut ranges);
    if ranges.is_empty() {
        return Cow::Borrowed(source);
    }
    let mut text = String::new();
    let mut previous = 0;
    for range in ranges {
        text.push_str(&source[previous..range.start]);
        previous = range.end;
    }
    text.push_str(&source[previous..]);
    Cow::Owned(text)
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

fn image<'a>(
    node: &'a Node,
    definition: impl Fn(&str) -> Option<&'a str>,
) -> Option<(&'a str, &'a str)> {
    match node {
        Node::Image(image) => Some((&image.url, &image.alt)),
        Node::ImageReference(image) => {
            definition(&image.identifier).map(|url| (url, image.alt.as_str()))
        }
        _ => None,
    }
}

/// Every image in reading order, including references.
pub fn markdown_images(source: &str) -> Vec<String> {
    fn visit(node: &Node, definitions: &HashMap<&str, &str>, images: &mut Vec<String>) {
        if let Some((url, _)) = image(node, |id| definitions.get(id).copied()) {
            images.push(url.to_owned());
        }
        for child in node.children().into_iter().flatten() {
            visit(child, definitions, images);
        }
    }
    let source = without_disclosure_tags(source);
    let mut images = Vec::new();
    // Inline and reference images both start with `![`; most text needs no parse.
    if source.contains("![") {
        let root = parse(&source);
        visit(&root, &definitions(&root), &mut images);
    }
    images
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn markdown_blocks(source: String) -> Vec<MarkdownBlock> {
    document(&source, None)
}

/// File links and code paths as workspace chips. A chip's
/// run names the file, resolved against `cwd` and the Host's `home`, with a parent
/// where another chip shares its basename; an authored caption stays as text before it.
pub fn workspace_markdown_blocks(
    source: String,
    cwd: String,
    home: Option<String>,
) -> Vec<MarkdownBlock> {
    let mut blocks = document(
        &source,
        Some(links::chips::Workspace::new(&cwd, home.as_deref())),
    );
    links::chips::label_parents(&mut blocks);
    blocks
}

/// What inline content reads besides its node: the document's link
/// definitions, and the workspace desktop chips resolve against.
struct Context<'a> {
    definitions: HashMap<&'a str, &'a str>,
    workspace: Option<links::chips::Workspace<'a>>,
}

fn document(source: &str, workspace: Option<links::chips::Workspace<'_>>) -> Vec<MarkdownBlock> {
    let source = without_disclosure_tags(source);
    let root = parse(&source);
    let context = Context {
        definitions: definitions(&root),
        workspace,
    };
    let mut blocks = Vec::new();
    block(
        &root,
        &source,
        &context,
        MarkdownStyle::default(),
        &mut blocks,
    );
    blocks
}

/// CSV always quotes cells, including embedded delimiters, quotes and newlines.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn markdown_table_csv(rows: Vec<Vec<String>>) -> String {
    let mut csv = String::new();
    for (row_index, row) in rows.into_iter().enumerate() {
        if row_index > 0 {
            csv.push_str("\r\n");
        }
        for (column, cell) in row.into_iter().enumerate() {
            if column > 0 {
                csv.push(',');
            }
            csv.push('"');
            for (quote, part) in cell.split('"').enumerate() {
                if quote > 0 {
                    csv.push_str("\"\"");
                }
                csv.push_str(part);
            }
            csv.push('"');
        }
    }
    csv
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

fn inline(node: &Node, context: &Context<'_>, mut style: MarkdownRun, runs: &mut Vec<MarkdownRun>) {
    let definitions = &context.definitions;
    if let Some((url, alt)) = image(node, |id| definitions.get(id).copied()) {
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
        && let Some(workspace) = &context.workspace
        && let Some(authored) = links::file_reference(destination, links::Syntax::Link)
        && let Some(file) = workspace.file(destination, links::Syntax::Link)
    {
        // The desktop client keeps an authored description as text before the chip,
        // with its own inline formatting; both open the file.
        if !links::is_file_label(&links::caption_text(node), &authored) {
            let caption = MarkdownRun {
                link: Some(destination.into()),
                ..style.clone()
            };
            for child in node.children().into_iter().flatten() {
                inline(child, context, caption.clone(), runs);
            }
        }
        runs.push(MarkdownRun {
            text: file.label.clone(),
            link: Some(destination.into()),
            file: Some(file),
            ..style
        });
        return;
    }
    if let Some(destination) = destination
        && context.workspace.is_none()
        && let Some(mut file) = links::file_reference(destination, links::Syntax::Link)
    {
        // Keep authored descriptions, as T3 Code does beside its file chips.
        // Native chips use that description as their label instead of an opaque basename.
        let caption = links::caption_text(node);
        if !links::is_file_label(&caption, &file) {
            file.label = caption;
        }
        runs.push(MarkdownRun {
            text: file.label.clone(),
            link: Some(destination.into()),
            file: Some(file),
            ..style
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
                style.file = match &context.workspace {
                    // A desktop chip takes the code span's place.
                    Some(workspace) => workspace
                        .file(&code.value, links::Syntax::DesktopCode)
                        .inspect(|_| style.code = false),
                    None => links::file_reference(&code.value, links::Syntax::MobileCode),
                };
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
            inline(child, context, style.clone(), runs);
        }
    } else if !style.text.is_empty() {
        runs.push(style);
    }
}

fn block(
    node: &Node,
    source: &str,
    context: &Context<'_>,
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
                                context,
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
                style,
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
                block(child, source, context, item_style, blocks);
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
        blocks.push(MarkdownBlock::Visualization { path, style });
        return;
    }
    if matches!(node, Node::Paragraph(_) | Node::Heading(_) | Node::Html(_)) {
        let mut runs = Vec::new();
        inline(node, context, MarkdownRun::default(), &mut runs);
        blocks.push(MarkdownBlock::Paragraph { runs, style });
    } else {
        for child in node.children().into_iter().flatten() {
            block(child, source, context, style.clone(), blocks);
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
    fn csv_preserves_empty_rows_and_empty_cells() {
        for (rows, expected) in [
            (vec![], ""),
            (vec![vec![]], ""),
            (vec![vec![], vec![]], "\r\n"),
            (vec![vec![], vec![""], vec![]], "\r\n\"\"\r\n"),
            (vec![vec!["", ""], vec![]], "\"\",\"\"\r\n"),
        ] {
            let rows = rows
                .into_iter()
                .map(|row| row.into_iter().map(str::to_owned).collect())
                .collect();
            assert_eq!(markdown_table_csv(rows), expected);
        }
    }

    #[test]
    fn csv_doubles_adjacent_quotes_and_keeps_unicode_and_line_endings() {
        assert_eq!(markdown_table_csv(vec![vec!["\"".into()]]), "\"\"\"\"");
        assert_eq!(
            markdown_table_csv(vec![vec!["a\"\"b".into()]]),
            "\"a\"\"\"\"b\""
        );
        assert_eq!(
            markdown_table_csv(vec![
                vec!["日,本".into(), "a\r\nb".into()],
                vec!["a\0b".into()]
            ]),
            "\"日,本\",\"a\r\nb\"\r\n\"a\0b\""
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
                path: "/fixture/icon-options.html".into(),
                style: MarkdownStyle::default(),
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
                &MarkdownBlock::Visualization { path, style: MarkdownStyle::default() },
            );
        }
    }

    #[test]
    fn tables_and_visualizations_keep_their_containing_list_style() {
        let source = "1. Parent\n\n   | A |\n   |---|\n   | B |\n\n   visualize{\"path\":\"x.html\"}\n\n   Continued\n2. Next";
        let blocks = markdown_blocks(source.into());
        let MarkdownBlock::Table { style, .. } = &blocks[1] else {
            panic!("missing nested table")
        };
        assert_eq!(style.list_depth, 1);
        assert!(style.marker.is_none());
        let MarkdownBlock::Visualization { style, .. } = &blocks[2] else {
            panic!("missing nested visualization")
        };
        assert_eq!(style.list_depth, 1);
        assert!(style.marker.is_none());
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
    fn image_free_markdown_lists_no_images() {
        for source in [
            "",
            "Hello! **日本語** [link](target)",
            r"!\[escaped](image.png)",
            "# Heading\n\n> quoted\n\n- list\n\n`code`",
            "[picture]: image.png\n\n[picture]",
            "&#33;[alt](image.png)",
            "!&#91;alt](image.png)",
            TABLE,
        ] {
            assert!(markdown_images(source).is_empty());
        }
    }

    #[test]
    fn disclosure_captions_and_images_are_visible_without_html_wrappers() {
        for separator in ["", "\n", "\n\n", "\r\n"] {
            for (open, summary, close_summary, close) in [
                ("<details>", "<summary>", "</summary>", "</details>"),
                (
                    "<DETAILS open data-note='a>b'>",
                    "<SUMMARY class='caption'>",
                    "</SUMMARY>",
                    "</DETAILS>",
                ),
            ] {
                let source = format!(
                    "Before\n\n{open}{separator}{summary}**東映の解約完了画面**{close_summary}{separator}![証拠][receipt]{separator}{close}\n\nAfter\n\n[receipt]: images/receipt.png"
                );
                let blocks = markdown_blocks(source.clone());
                let runs: Vec<_> = blocks
                    .iter()
                    .flat_map(|block| match block {
                        MarkdownBlock::Paragraph { runs, .. } => runs.as_slice(),
                        _ => panic!("unexpected block"),
                    })
                    .collect();
                assert_eq!(
                    runs.iter()
                        .filter_map(|run| run.image.as_deref())
                        .collect::<Vec<_>>(),
                    ["images/receipt.png"]
                );
                assert!(
                    runs.iter()
                        .any(|run| run.text == "東映の解約完了画面" && run.strong)
                );
                let text: String = runs.iter().map(|run| run.text.as_str()).collect();
                assert_eq!(
                    text.replace(['\r', '\n'], ""),
                    "Before東映の解約完了画面証拠After"
                );
                assert_eq!(markdown_images(&source), ["images/receipt.png"]);
                let rendered = without_disclosure_tags(&source);
                assert!(!rendered.contains("<"));
                let caption = rendered.find("**東映の解約完了画面**").unwrap();
                let image = rendered.find("![証拠][receipt]").unwrap();
                assert!(caption < image && image < rendered.find("After").unwrap());
            }
        }
        let source = "<details><summary>説明だけ</summary>本文</details>";
        assert_eq!(without_disclosure_tags(source), "説明だけ本文");
        assert_eq!(
            paragraph_text(&markdown_blocks(source.into())[0]),
            "説明だけ本文"
        );
        assert_eq!(without_disclosure_tags("<details/>本文<summary/>"), "本文");
    }

    #[test]
    fn disclosure_wrappers_preserve_code_and_nested_contents() {
        let source = "<details>\n<summary>外側</summary>\n```html\n<summary>![literal](code.png)</summary>\n```\n<details>\n<summary>内側</summary>\n\n![画像](nested.png)\n</details>\n</details>";
        let blocks = markdown_blocks(source.into());
        let code = blocks
            .iter()
            .find(|block| {
                matches!(block,
                    MarkdownBlock::Paragraph { style, .. } if style.code
                )
            })
            .expect("code remains code inside a disclosure");
        assert_eq!(
            paragraph_text(code),
            "<summary>![literal](code.png)</summary>"
        );
        assert!(blocks.iter().any(|block| paragraph_text(block) == "外側"));
        assert!(
            blocks
                .iter()
                .any(|block| paragraph_text(block).trim() == "内側")
        );
        assert_eq!(markdown_images(source), ["nested.png"]);
        assert_eq!(
            without_disclosure_tags(source),
            "\n外側\n```html\n<summary>![literal](code.png)</summary>\n```\n\n内側\n\n![画像](nested.png)\n\n"
        );
    }

    #[test]
    fn disclosure_tags_in_literals_and_unrelated_html_are_unchanged() {
        for source in [
            "`<details>`",
            "`<summary>`",
            "```html\n<details><summary>![literal](image.png)</summary></details>\n```",
            r"\<details>\<summary>literal\</summary>\</details>",
            "<details-extra>literal</details-extra>",
            "<summary-note>literal</summary-note>",
            "<script>const example = '<details>![literal](image.png)</details>';</script>",
            "<details unfinished='attribute",
            "<summary unfinished='attribute",
            "<div>![literal](image.png)</div>",
        ] {
            let rendered = without_disclosure_tags(source);
            assert!(matches!(rendered, Cow::Borrowed(_)), "{source}");
            assert_eq!(rendered, source);
            assert!(markdown_images(source).is_empty());
            assert_eq!(
                paragraph_text(&markdown_blocks(source.into())[0]),
                match source {
                    source if source.starts_with("```") => source
                        .strip_prefix("```html\n")
                        .unwrap()
                        .strip_suffix("\n```")
                        .unwrap(),
                    source if source.starts_with('`') => source.trim_matches('`'),
                    source if source.starts_with('\\') =>
                        "<details><summary>literal</summary></details>",
                    source => source,
                }
            );
        }
    }

    #[test]
    fn disclosure_images_remain_valid_at_every_stream_boundary() {
        let source = "[receipt]: images/receipt.png\n\n<details>\n<summary>日本語の説明🌸</summary>\n\n![証拠][receipt]\n\n</details>";
        for end in source
            .char_indices()
            .map(|(end, _)| end)
            .chain([source.len()])
        {
            let partial = &source[..end];
            let images = markdown_images(partial);
            assert!(images.len() <= 1);
            assert!(images.iter().all(|url| url == "images/receipt.png"));
            markdown_blocks(partial.into());
        }
        assert_eq!(markdown_images(source), ["images/receipt.png"]);
    }

    #[test]
    fn literal_image_openers_are_not_images() {
        for source in [
            r"\![escaped](image.png)",
            "`![inline code](image.png)`",
            "```markdown\n![fenced code](image.png)\n```",
            "![unfinished](",
            "![undefined reference]",
        ] {
            assert!(markdown_images(source).is_empty());
        }
    }

    #[test]
    fn every_image_reference_form_is_listed() {
        for image in [
            "![alt](image.png)",
            "![alt][picture]",
            "![picture][]",
            "![picture]",
        ] {
            let source = format!("前 {image} 後\n\n[picture]: image.png");
            assert_eq!(markdown_images(&source), ["image.png"]);
        }
    }

    #[test]
    fn images_are_listed_in_reading_order_with_their_references() {
        let source = include_str!("../../tests/fixtures/markdown/document.md");
        assert_eq!(markdown_images(source), ["images/example.png"]);
        let source =
            "![first](a.png) **![second](b.png)** [![third][image]](target)\n\n[image]: c.png";
        assert_eq!(markdown_images(source), ["a.png", "b.png", "c.png"]);
        assert!(markdown_images(TABLE).is_empty());
    }
}
