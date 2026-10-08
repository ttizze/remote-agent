//! Chat Markdown: the look of assistant text, plans and sent prompts, and
//! where their links lead.
use super::super::{
    Desktop,
    panel::PanelTab,
    ui::{color, tint},
};
use agent_core::{
    presentation::markdown::{
        artifact_templates::{
            append_artifact_template_use_prompt, artifact_template_presentation_label,
        },
        directives::{
            ArtifactTemplateMarkdownSegment, render_file_citations_as_markdown,
            split_artifact_template_markdown,
        },
        links::{MarkdownLinkPresentation, markdown_link_presentation},
    },
    state::Intent,
    view::composer::chips::{ContextChip, ContextChipKind},
};
use gpui_kit::{
    component::text::{TextView, TextViewStyle},
    *,
};
use std::{path::Path, sync::Arc};

const CONTEXT_LINK: &str = "context://v1/";

fn style() -> TextViewStyle {
    let word_wrap = super::super::ui_word_wrap();
    let mut code_block = StyleRefinement::default()
        .bg(color("secondary"))
        .border_1()
        .border_color(tint("border", 0.7))
        .rounded(px(10.));
    code_block.text.font_size = Some(px(super::super::ui::metrics().code_size).into());
    code_block.text.white_space = Some(if word_wrap {
        WhiteSpace::Normal
    } else {
        WhiteSpace::Nowrap
    });
    let mut table = StyleRefinement::default();
    let mut table_cell = StyleRefinement::default();
    if !word_wrap {
        table.overflow.x = Some(Overflow::Scroll);
        table_cell.text.white_space = Some(WhiteSpace::Nowrap);
    }
    TextViewStyle::default()
        .paragraph_gap(rems(0.65))
        .heading_font_size(|level, base| match level {
            1 => px(20.),
            2 => px(18.),
            3 => px(16.),
            _ => base,
        })
        .code_block(code_block)
        .table(table)
        .table_cell(table_cell)
        .inline_code(HighlightStyle {
            background_color: Some(tint("text", 0.06)),
            ..HighlightStyle::default()
        })
}

/// Markdown at the prompt size with relaxed lines; links open in the right
/// place, and `chips` resolve the context links of a sent prompt.
pub(super) fn chat_markdown(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    chips: Arc<Vec<ContextChip>>,
    cx: &mut Context<Desktop>,
) -> Stateful<Div> {
    let id = id.into();
    let text = text.into();
    let mut body = div().id(id.clone());
    for (index, segment) in split_artifact_template_markdown(&text)
        .into_iter()
        .enumerate()
    {
        let key = SharedString::from(format!("{id:?}-segment-{index}"));
        match segment {
            ArtifactTemplateMarkdownSegment::Markdown { markdown, .. } => {
                body = body.child(markdown_text(
                    key,
                    render_file_citations_as_markdown(&markdown),
                    chips.clone(),
                    cx,
                ));
            }
            ArtifactTemplateMarkdownSegment::ArtifactTemplate { template, .. } => {
                let label = artifact_template_presentation_label(template.artifact_kind);
                let name = template.display_name.clone();
                let button = gpui_kit::component::button::Button::new(key)
                    .label("Use template")
                    .on_click(cx.listener(move |view, _, window, cx| {
                        let (draft, _) = view.editor_text_and_cursor(cx);
                        let next = append_artifact_template_use_prompt(draft, template.clone());
                        view.replace_composer_text(next, window, cx);
                    }));
                body = body.child(
                    div()
                        .my(px(10.))
                        .p(px(12.))
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .border_1()
                        .border_color(color("border"))
                        .rounded(px(12.))
                        .bg(color("card"))
                        .child(
                            div().flex_1().child(div().text_sm().child(name)).child(
                                div()
                                    .text_xs()
                                    .text_color(color("secondaryLabel"))
                                    .child(label),
                            ),
                        )
                        .child(button),
                );
            }
        }
    }
    body
}

fn markdown_text(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    chips: Arc<Vec<ContextChip>>,
    cx: &mut Context<Desktop>,
) -> TextView {
    let owner = cx.entity().downgrade();
    TextView::markdown(id, text)
        .selectable(true)
        .style(style())
        .on_link_click(move |href, _, window, cx| {
            let href = href.to_string();
            let chip = href
                .strip_prefix(CONTEXT_LINK)
                .and_then(|reference| reference.rsplit('/').next())
                .and_then(|context_id| chips.iter().find(|chip| chip.context_id == context_id))
                .cloned();
            let _ = owner.update(cx, |view, cx| match chip {
                Some(chip) => view.open_context_chip(&chip, window, cx),
                None => view.open_markdown_link(&href, window, cx),
            });
        })
}

impl Desktop {
    /// Web links open in the browser; file links open in the Files panel.
    fn open_markdown_link(&mut self, href: &str, window: &mut Window, cx: &mut Context<Self>) {
        match markdown_link_presentation(href) {
            MarkdownLinkPresentation::External { href, .. }
            | MarkdownLinkPresentation::Link { href: Some(href) } => cx.open_url(&href),
            MarkdownLinkPresentation::File { link } => {
                self.open_workspace_file(link.path, link.line, window, cx)
            }
            MarkdownLinkPresentation::Link { href: None } => {}
        }
    }

    /// Opens a file of the thread's workspace in the right panel, at `line`
    /// (from 1) when the link names one.
    pub(super) fn open_workspace_file(
        &mut self,
        path: String,
        line: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = if Path::new(&path).is_absolute() {
            path
        } else {
            let Some(thread) = self.snapshot.selected_thread.clone() else {
                return;
            };
            Path::new(&self.snapshot.thread_cwd(&thread))
                .join(&path)
                .to_string_lossy()
                .into_owned()
        };
        self.open_right_panel(PanelTab::Files, window, cx);
        self.reveal_file_line(path.clone(), line);
        self.perform(Intent::ReadFile {
            path,
            discard_draft: false,
        });
    }

    /// What a context chip in a sent prompt opens.
    fn open_context_chip(
        &mut self,
        chip: &ContextChip,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match chip.kind {
            ContextChipKind::Mention => {
                if let Some(path) = chip.path.clone() {
                    self.open_workspace_file(path, None, window, cx);
                }
            }
            ContextChipKind::Thread => {
                if let Some(thread) = chip.thread_id.clone() {
                    self.open_thread(thread, cx);
                }
            }
            ContextChipKind::Image | ContextChipKind::Video | ContextChipKind::File => {
                if let Some(attachment) = chip.attachment_id.clone() {
                    self.save_attachment(attachment, chip.label.clone());
                }
            }
            _ => {
                if let Some(url) = &chip.url {
                    cx.open_url(url);
                }
            }
        }
    }
}
