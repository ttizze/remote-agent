use super::{MarkdownCallout, Node, parse, parse_options};
use std::{borrow::Cow, ops::Range};

struct Tag {
    range: Range<usize>,
    summary: bool,
    closing: bool,
    open: bool,
}

pub(super) struct Disclosure {
    pub range: Range<usize>,
    pub summary: Range<usize>,
    pub body: [Range<usize>; 2],
    pub open: bool,
}

fn tag(value: &str, range: Range<usize>) -> Option<Tag> {
    let value = value.strip_prefix('<')?;
    let closing = value.starts_with('/');
    let value = value.strip_prefix('/').unwrap_or(value);
    let end = value.find(|c: char| c.is_ascii_whitespace() || matches!(c, '/' | '>'))?;
    let name = &value[..end];
    if !name.eq_ignore_ascii_case("details") && !name.eq_ignore_ascii_case("summary") {
        return None;
    }
    Some(Tag {
        range,
        summary: name.eq_ignore_ascii_case("summary"),
        closing,
        open: has_open_attribute(&value[end..]),
    })
}

// HTML boolean attributes are true by presence, including open="false".
// Attribute values may contain >, whitespace, or the word "open".
fn has_open_attribute(mut attributes: &str) -> bool {
    while !attributes.is_empty() {
        attributes = attributes.trim_start_matches(|c: char| c.is_ascii_whitespace());
        let end = attributes
            .find(|c: char| c.is_ascii_whitespace() || matches!(c, '=' | '/' | '>'))
            .unwrap_or(attributes.len());
        if end == 0 {
            return false;
        }
        if attributes[..end].eq_ignore_ascii_case("open") {
            return true;
        }
        attributes = attributes[end..].trim_start_matches(|c: char| c.is_ascii_whitespace());
        if let Some(value) = attributes.strip_prefix('=') {
            let value = value.trim_start_matches(|c: char| c.is_ascii_whitespace());
            attributes = match value.chars().next() {
                Some(quote @ ('\'' | '"')) => {
                    value[1..].find(quote).map_or("", |end| &value[end + 2..])
                }
                _ => value
                    .find(|c: char| c.is_ascii_whitespace() || c == '>')
                    .map_or("", |end| &value[end..]),
            };
        }
    }
    false
}

fn tags(source: &str) -> Vec<Tag> {
    if !source
        .match_indices('<')
        .any(|(offset, _)| tag(&source[offset..], 0..0).is_some())
    {
        return Vec::new();
    }
    fn collect(node: &Node, base: usize, output: &mut Vec<Tag>) {
        if let Node::Html(html) = node {
            let position = node.position().expect("HTML has source positions");
            if let Some(tag) = tag(
                &html.value,
                base + position.start.offset..base + position.end.offset,
            ) {
                output.push(tag);
            }
        }
        for child in node.children().into_iter().flatten() {
            collect(child, base, output);
        }
    }
    fn visit(node: &Node, source: &str, output: &mut Vec<Tag>) {
        if let Node::Html(html) = node {
            let position = node.position().expect("HTML has source positions");
            if tag(html.value.trim_start(), 0..0).is_some() {
                let mut options = parse_options();
                options.constructs.html_flow = false;
                let fragment = ::markdown::to_mdast(
                    &source[position.start.offset..position.end.offset],
                    &options,
                )
                .expect("GFM is infallible");
                collect(&fragment, position.start.offset, output);
            }
            return;
        }
        for child in node.children().into_iter().flatten() {
            visit(child, source, output);
        }
    }
    let mut output = Vec::new();
    if source.contains('<') {
        visit(&parse(source), source, &mut output);
    }
    output
}

pub(super) fn without_disclosure_tags(source: &str) -> Cow<'_, str> {
    let tags = tags(source);
    if tags.is_empty() {
        return Cow::Borrowed(source);
    }
    let mut text = String::new();
    let mut previous = 0;
    for tag in tags {
        text.push_str(&source[previous..tag.range.start]);
        previous = tag.range.end;
    }
    text.push_str(&source[previous..]);
    Cow::Owned(text)
}

/// Only outer containers are returned. A recursive document pass parses their
/// bodies. An unfinished stream keeps the remaining body rather than hiding it.
pub(super) fn disclosures(source: &str) -> Vec<Disclosure> {
    let tags = tags(source);
    let mut output = Vec::new();
    let mut depth = 0;
    let mut start = None;
    for (index, tag) in tags.iter().enumerate() {
        if tag.summary {
            continue;
        }
        if !tag.closing {
            if depth == 0 {
                start = Some(index);
            }
            depth += 1;
        } else if depth > 0 {
            depth -= 1;
            if depth == 0 {
                output.push(disclosure(
                    &tags[start.take().unwrap()..=index],
                    tag.range.start,
                    tag.range.end,
                ));
            }
        }
    }
    if let Some(start) = start {
        output.push(disclosure(&tags[start..], source.len(), source.len()));
    }
    output
}

fn disclosure(tags: &[Tag], body_end: usize, end: usize) -> Disclosure {
    let first = &tags[0];
    let mut summary = first.range.end..first.range.end;
    let mut body = [first.range.end..first.range.end, first.range.end..body_end];
    // Summary belongs to this details element, not to a nested disclosure.
    if let Some(open) = tags.get(1).filter(|tag| tag.summary && !tag.closing) {
        let close = tags.iter().skip(2).find(|tag| tag.summary && tag.closing);
        summary = open.range.end..close.map_or(body_end, |tag| tag.range.start);
        body = [
            first.range.end..open.range.start,
            close.map_or(body_end, |tag| tag.range.end)..body_end,
        ];
    }
    Disclosure {
        range: first.range.start..end,
        summary,
        body,
        open: first.open,
    }
}

pub(super) fn callout(
    quote: &::markdown::mdast::Blockquote,
    source: &str,
) -> Option<(MarkdownCallout, Vec<Node>)> {
    let Node::Paragraph(paragraph) = quote.children.first()? else {
        return None;
    };
    let Node::Text(text) = paragraph.children.first()? else {
        return None;
    };
    let (marker, rest) = text.value.split_once('\n').unwrap_or((&text.value, ""));
    let marker_ends_line = text.value.contains('\n');
    if rest.is_empty() && !marker_ends_line && paragraph.children.len() > 1 {
        return None;
    }
    let kind = match marker.trim_end_matches('\r').to_ascii_uppercase().as_str() {
        "[!NOTE]" => MarkdownCallout::Note,
        "[!TIP]" => MarkdownCallout::Tip,
        "[!IMPORTANT]" => MarkdownCallout::Important,
        "[!WARNING]" => MarkdownCallout::Warning,
        "[!CAUTION]" => MarkdownCallout::Caution,
        _ => return None,
    };
    let mut children = quote.children.clone();
    if let Node::Paragraph(paragraph) = &mut children[0] {
        // Literal-dollar fallback only reuses byte offsets. Keep the consumed
        // marker outside that range; the leading newline parses as whitespace.
        if marker_ends_line
            && let Some(position) = &mut paragraph.position
            && let Some(newline) = source[position.start.offset..position.end.offset].find('\n')
        {
            position.start.offset += newline;
        }
        if let Node::Text(text) = &mut paragraph.children[0] {
            text.value = rest.into();
        }
        if rest.is_empty() {
            paragraph.children.remove(0);
        }
        if paragraph.children.is_empty() {
            children.remove(0);
        }
    }
    Some((kind, children))
}

#[cfg(test)]
mod tests {
    use super::super::{
        MarkdownBlock, MarkdownCallout, markdown_blocks, markdown_images, markdown_runs,
        workspace_markdown_blocks,
    };

    fn text(blocks: &[MarkdownBlock]) -> String {
        markdown_runs(blocks)
            .iter()
            .map(|run| run.text.as_str())
            .collect()
    }

    #[test]
    fn math_retains_source_and_distinguishes_inline_display_and_code() {
        let blocks = markdown_blocks(
            "値は $x^2 + 日本語$。\n\n$$\n\\frac{a}{b}\n$$\n\n`$literal$`\n\n```tex\n$code$\n```"
                .into(),
        );
        let runs = markdown_runs(&blocks);
        let math: Vec<_> = runs
            .iter()
            .filter(|run| run.math)
            .map(|run| run.text.as_str())
            .collect();
        assert_eq!(math, ["x^2 + 日本語", "\\frac{a}{b}"]);
        assert!(
            matches!(&blocks[1], MarkdownBlock::Paragraph { style, .. } if style.math && !style.code)
        );
        assert!(
            runs.iter()
                .any(|run| run.text == "$literal$" && run.code && !run.math)
        );
        let MarkdownBlock::Paragraph { runs: code, style } = blocks.last().unwrap() else {
            panic!("code block");
        };
        assert!(style.code && !style.math);
        assert_eq!(
            code.iter().map(|run| run.text.as_str()).collect::<String>(),
            "$code$"
        );
        assert!(code.iter().all(|run| !run.math));
        assert_eq!(
            text(&markdown_blocks("unfinished $日本語".into())),
            "unfinished $日本語"
        );
        let fenced = markdown_blocks("```math\nx^2\n```".into());
        assert!(
            matches!(&fenced[0], MarkdownBlock::Paragraph { style, .. } if style.math && !style.code)
        );
        for source in [
            "$fixture-review costs $5",
            "$first-skill $second-skill",
            "$HOME/file and $PATH/bin",
        ] {
            let blocks = markdown_blocks(source.into());
            assert_eq!(text(&blocks), source);
            assert!(markdown_runs(&blocks).iter().all(|run| !run.math));
        }
        let mixed = markdown_blocks("$x$ costs $5 or $10".into());
        assert!(
            markdown_runs(&mixed)
                .iter()
                .any(|run| run.math && run.text == "x")
        );
        assert!(text(&mixed).contains("$5 or $10"));
        let mixed = markdown_blocks("$x$ $fixture-review **bold** costs $5".into());
        let runs = markdown_runs(&mixed);
        assert!(runs.iter().any(|run| run.math && run.text == "x"));
        assert!(runs.iter().any(|run| run.strong && run.text == "bold"));
        assert_eq!(text(&mixed), "x $fixture-review bold costs $5");
        for source in ["$x$です", "$x$coefficient", "$x + y $!"] {
            assert!(
                markdown_runs(&markdown_blocks(source.into()))
                    .iter()
                    .any(|run| run.math),
                "{source}"
            );
        }
    }

    #[test]
    fn every_callout_kind_keeps_its_formatted_body_and_normal_quotes_stay_quotes() {
        for (name, kind) in [
            ("NOTE", MarkdownCallout::Note),
            ("TIP", MarkdownCallout::Tip),
            ("IMPORTANT", MarkdownCallout::Important),
            ("WARNING", MarkdownCallout::Warning),
            ("CAUTION", MarkdownCallout::Caution),
        ] {
            assert_eq!(
                super::super::markdown_callout_marker(kind),
                format!("[!{name}]")
            );
            assert!(
                matches!(&markdown_blocks(format!("> [!{name}]"))[0], MarkdownBlock::Callout { blocks, .. } if blocks.is_empty())
            );
            let blocks = markdown_blocks(format!("> [!{name}]\n> **本文** $x$\n>\n> - task"));
            let [
                MarkdownBlock::Callout {
                    kind: actual,
                    blocks: body,
                    ..
                },
            ] = blocks.as_slice()
            else {
                panic!("missing callout");
            };
            assert_eq!(*actual, kind);
            assert!(
                markdown_runs(body)
                    .iter()
                    .any(|run| run.strong && run.text == "本文")
            );
            assert!(text(body).contains("task"));
            assert!(!text(body).contains(&format!("[!{name}]")));
        }
        assert!(matches!(
            &markdown_blocks("> [!note]\n> lower case".into())[0],
            MarkdownBlock::Callout {
                kind: MarkdownCallout::Note,
                ..
            }
        ));
        let blocks = markdown_blocks("> [!NOTE]\n> $fixture-review **bold** costs $5".into());
        let [
            MarkdownBlock::Callout {
                blocks: body,
                style,
                ..
            },
        ] = blocks.as_slice()
        else {
            panic!("callout");
        };
        assert!(style.quoted);
        assert_eq!(text(body), "$fixture-review bold costs $5");
        assert!(
            markdown_runs(body)
                .iter()
                .any(|run| run.strong && run.text == "bold")
        );
        for source in [
            "> [!UNKNOWN]\n> body",
            "> `[!NOTE]`\n> body",
            "> [!NOTE] inline\n> body",
            "> [!NOTE]**inline**\n> body",
            "> [!NOTE]  \n> body",
        ] {
            let blocks = markdown_blocks(source.into());
            assert!(matches!(&blocks[0], MarkdownBlock::Paragraph { style, .. } if style.quoted));
            assert!(text(&blocks).contains("[!"));
        }
    }

    #[test]
    fn disclosure_boolean_attributes_ignore_open_inside_other_values() {
        for (attributes, expected) in [
            (" class=card open", true),
            (" class=open", false),
            (" class=card\tOPEN=false", true),
            (" data-note='a > open'", false),
            (" data-open=yes", false),
            (" class=card", false),
            (" class=card data-other=ok", false),
        ] {
            let blocks = markdown_blocks(format!(
                "<details{attributes}><summary>caption</summary>body</details>"
            ));
            assert!(
                matches!(&blocks[0], MarkdownBlock::Disclosure { open, .. } if *open == expected),
                "{attributes}"
            );
        }
    }

    #[test]
    fn nested_disclosures_preserve_open_attributes_summary_formatting_and_shared_file_labels() {
        let source = "Before\n\n<DETAILS data-note='open a>b' OPEN=\"false\"><SUMMARY>**表題** [index.ts](a/index.ts)</SUMMARY>\n\n<details><summary>Inner</summary>\n\n[index.ts](b/index.ts)\n\n</details>\n\n![proof][image]\n\n</DETAILS>\n\nAfter\n\n[image]: evidence.png";
        let blocks = workspace_markdown_blocks(source.into(), "/repo".into(), None);
        let MarkdownBlock::Disclosure {
            summary,
            open,
            blocks: body,
            ..
        } = &blocks[1]
        else {
            panic!("missing details");
        };
        assert!(*open);
        assert!(summary.iter().any(|run| run.text == "表題" && run.strong));
        let MarkdownBlock::Disclosure {
            open,
            blocks: inner,
            ..
        } = &body[0]
        else {
            panic!("missing nested details");
        };
        assert!(!*open);
        assert!(text(inner).contains("index.ts"));
        let labels: Vec<_> = markdown_runs(&blocks)
            .iter()
            .filter_map(|run| run.file.as_ref().map(|file| file.label.clone()))
            .collect();
        assert_eq!(labels, ["index.ts · repo/a", "index.ts · repo/b"]);
        assert_eq!(
            text(&blocks),
            "Before表題 index.ts · repo/aInnerindex.ts · repo/bproofAfter"
        );
        assert_eq!(markdown_images(source), ["evidence.png"]);
    }

    #[test]
    fn tags_inside_math_stay_literal_inside_a_real_disclosure() {
        let blocks = markdown_blocks("<details><summary>Caption</summary>\n\n$\\text{<details>literal</details>}$\n\n</details>".into());
        let [MarkdownBlock::Disclosure { blocks: body, .. }] = blocks.as_slice() else {
            panic!("details");
        };
        assert_eq!(body.len(), 1);
        let runs = markdown_runs(body);
        assert_eq!(runs.len(), 1);
        assert!(runs[0].math);
        assert_eq!(runs[0].text, "\\text{<details>literal</details>}");
    }

    #[test]
    fn streaming_and_deep_disclosures_keep_their_contents_without_inventing_open_state() {
        let blocks =
            markdown_blocks("<details>before<summary>Caption</summary>after</details>".into());
        let [
            MarkdownBlock::Disclosure {
                summary,
                blocks: body,
                ..
            },
        ] = blocks.as_slice()
        else {
            panic!("details");
        };
        assert_eq!(
            summary
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>(),
            "Caption"
        );
        assert_eq!(text(body), "beforeafter");
        let source = "<details data-label='open'><summary>Caption</summary>\n\nbody\n\n![a](a.png)";
        let blocks = markdown_blocks(source.into());
        assert!(matches!(
            &blocks[0],
            MarkdownBlock::Disclosure { open: false, .. }
        ));
        assert_eq!(text(&blocks), "Captionbodya");
        let source = format!(
            "{}payload{}",
            "<details><summary>x</summary>".repeat(100),
            "</details>".repeat(100)
        );
        assert!(text(&markdown_blocks(source)).contains("payload"));
        for source in [
            "`<details><summary>code</summary></details>`",
            "```html\n<details open>\n</details>\n```",
            r"\<details>escaped\</details>",
        ] {
            let blocks = markdown_blocks(source.into());
            assert!(
                blocks
                    .iter()
                    .all(|block| !matches!(block, MarkdownBlock::Disclosure { .. }))
            );
            assert!(text(&blocks).contains("<details"));
        }
    }
}
