//! Debug comparison with NSH's own Markdown parser, document layout and selection.
//! The conversation List remains the sole vertical scroller. This deliberately
//! uses TextView's default typography/code/table controls. Bex's file chips,
//! Mermaid rendering and find decorations are not ported into this experiment.
//! TextView eagerly lays out a whole message, unlike the custom part list.
//! Acceptance currently exposes link AX-press and code-copy AX-order gaps.
use super::{MarkdownBlock, MarkdownRun, MarkdownStyle};
use nsh_base::text_view::{Block, CodeBlock, Document, Image, Inline, Parser};

#[cfg(all(target_os = "macos", debug_assertions))]
use {
    super::{Kind, Surroundings, Target},
    nsh_base::{MarkdownOptions, TextViewCodeAction, TextViewLinkPressEvent, text_view},
    nsh_core::prelude::*,
};

#[cfg(all(target_os = "macos", debug_assertions))]
pub(super) fn message(
    surroundings: Surroundings,
    source: impl Fn() -> Option<String> + 'static,
    kind: impl Fn() -> Kind + Copy + 'static,
    streaming: impl Fn() -> bool + Copy + 'static,
    max_width: impl Fn() -> f32 + Copy + 'static,
) -> View {
    render(
        text_view()
            .markdown(move || SharedString::from(source().unwrap_or_default()))
            .options(move || MarkdownOptions {
                streaming: streaming(),
                ..Default::default()
            }),
        surroundings,
        kind,
        max_width,
    )
}

#[cfg(all(target_os = "macos", debug_assertions))]
pub(super) fn content(
    surroundings: Surroundings,
    blocks: impl Fn() -> Vec<MarkdownBlock> + 'static,
    kind: impl Fn() -> Kind + Copy + 'static,
    max_width: impl Fn() -> f32 + Copy + 'static,
) -> View {
    render(
        text_view().document(move || document(blocks())),
        surroundings,
        kind,
        max_width,
    )
}

#[cfg(all(target_os = "macos", debug_assertions))]
fn render(
    text: nsh_base::TextViewElement,
    surroundings: Surroundings,
    kind: impl Fn() -> Kind + Copy + 'static,
    max_width: impl Fn() -> f32 + Copy + 'static,
) -> View {
    let target = super::target(surroundings.connection);
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
        text.w_full()
            .min_w_0()
            .selectable(true)
            .code_block_actions(vec![
                TextViewCodeAction::new("copy").with_label(crate::l10n::tr("コピー")),
            ])
            .on_link_press(move |event: &TextViewLinkPressEvent| {
                if event.url.starts_with('#') {
                    return;
                }
                if let Ok(target) = serde_json::from_str::<Target>(&target.get_untracked()) {
                    surroundings.open_link(event.url.to_string(), target, false);
                }
            })
            .render_image(move |image: &Image| {
                let source = image.src.clone();
                crate::media::image_view(
                    surroundings.connection,
                    surroundings.preview,
                    move || source.clone(),
                    crate::media::ImageSize::Natural,
                    None,
                )
            }),
    )
}

/// Tool details already arrive as Core's blocks; retain their text and inline
/// semantics without constructing Bex's custom native/drawn paragraph adapter.
fn document(blocks: Vec<MarkdownBlock>) -> Document {
    Document::new(
        blocks
            .into_iter()
            .flat_map(|block| match block {
                MarkdownBlock::Paragraph { runs, style } => vec![paragraph(runs, style)],
                MarkdownBlock::Table { source, .. } => Parser::new().markdown(&source).blocks,
                MarkdownBlock::Visualization { path } => {
                    vec![Block::paragraph(vec![Inline::Link {
                        content: vec![Inline::text(&path)],
                        url: path,
                        title: None,
                    }])]
                }
            })
            .collect(),
    )
}

fn paragraph(runs: Vec<MarkdownRun>, style: MarkdownStyle) -> Block {
    if style.rule {
        return Block::Rule;
    }
    if style.code {
        return Block::CodeBlock(CodeBlock::new(
            style.language,
            runs.into_iter().map(|run| run.text).collect::<String>(),
        ));
    }
    let mut content: Vec<_> = runs.into_iter().map(inline).collect();
    if let Some(marker) = style.marker {
        content.insert(0, Inline::text(format!("{marker} ")));
    }
    let block = match style.header {
        Some(level) => Block::Heading {
            level,
            content,
            anchor: None,
        },
        None => Block::paragraph(content),
    };
    if style.quoted {
        Block::Blockquote {
            children: vec![block],
            alert: None,
        }
    } else {
        block
    }
}

fn inline(run: MarkdownRun) -> Inline {
    let mut node = if let Some(src) = run.image {
        let mut image = Image::new(src);
        image.alt = run.text;
        Inline::Image(image)
    } else if run.code {
        Inline::Code { code: run.text }
    } else {
        Inline::text(run.text)
    };
    if run.strong {
        node = Inline::Strong {
            content: vec![node],
        };
    }
    if run.emphasis {
        node = Inline::Emphasis {
            content: vec![node],
        };
    }
    if run.strikethrough {
        node = Inline::Strikethrough {
            content: vec![node],
        };
    }
    if let Some(url) = run.link {
        node = Inline::Link {
            url,
            title: None,
            content: vec![node],
        };
    }
    node
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_code_keeps_literal_characters_and_markdown_tables_stay_tables() {
        let doc = document(vec![
            MarkdownBlock::Paragraph {
                runs: vec![MarkdownRun {
                    text: "a < b && **literal**\n日本語".into(),
                    ..Default::default()
                }],
                style: MarkdownStyle {
                    code: true,
                    language: Some("rust".into()),
                    ..Default::default()
                },
            },
            MarkdownBlock::Table {
                source: "| A | B |\n|---|---|\n|日本語|`x`|".into(),
                columns: vec![],
                rows: vec![],
            },
        ]);
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
        let node = inline(MarkdownRun {
            text: "*literal*".into(),
            code: true,
            strong: true,
            link: Some("hello.txt".into()),
            ..Default::default()
        });
        let Inline::Link { url, content, .. } = node else {
            panic!("missing link")
        };
        assert_eq!(url, "hello.txt");
        assert!(
            matches!(&content[0], Inline::Strong { content } if matches!(&content[0], Inline::Code { code } if code == "*literal*"))
        );
    }
}
