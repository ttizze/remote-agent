//! Typed transcript context chooses its presentation and destinations here.
//! Labels and captured contents are data, never resource-operation authority.
use agent_protocol::{
    attribution::Author,
    context::{ContextContent, MessageContext},
};
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ContextTarget {
    File {
        path: String,
    },
    Thread {
        host: String,
        storage: String,
        session: crate::session::SessionRef,
    },
    Citation {
        citation: agent_protocol::context::AssistantCitation,
    },
    Web {
        url: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ContextLink {
    pub label: String,
    pub target: ContextTarget,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ContextSection {
    pub title: String,
    pub text: String,
    pub language: Option<String>,
}
impl ContextSection {
    fn text(title: &str, text: &str) -> Self {
        Self {
            title: title.into(),
            text: text.into(),
            language: None,
        }
    }
    fn code(title: &str, text: &str, language: &str) -> Self {
        Self {
            title: title.into(),
            text: text.into(),
            language: Some(language.into()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[serde(rename_all = "camelCase")]
pub struct CitationPreview {
    pub prefix: String,
    pub text: String,
    pub suffix: String,
    pub highlighted: bool,
}

impl CitationPreview {
    fn new(text: &str, prefix: &str, suffix: &str) -> Self {
        Self {
            prefix: if prefix.is_empty() {
                String::new()
            } else {
                format!("…{}", crate::text::collapse_spaces(prefix))
            },
            text: crate::text::collapse_spaces(text),
            suffix: if suffix.is_empty() {
                String::new()
            } else {
                format!("{}…", crate::text::collapse_spaces(suffix))
            },
            highlighted: !prefix.is_empty() || !suffix.is_empty(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[serde(rename_all = "camelCase")]
pub struct ContextPresentation {
    pub id: String,
    pub symbol: String,
    pub details: String,
    pub comment: Option<String>,
    pub citation: bool,
    pub icon_data_url: Option<String>,
    pub href: String,
    pub label: String,
    pub kind: String,
    pub accessibility_label: String,
    pub tooltip: Option<String>,
    pub action: Option<ContextLink>,
    pub sections: Vec<ContextSection>,
    pub image: Option<String>,
    pub searchable: bool,
    pub citation_preview: Option<CitationPreview>,
}

fn label(context: &MessageContext) -> &str {
    let label = crate::text::trim(&context.label);
    if label.is_empty() { "Context" } else { label }
}

/// This private document URI is resolved only against the row's typed records.
/// Percent-encoding preserves identities containing reserved punctuation.
pub fn context_href(context: &MessageContext) -> String {
    let encode =
        |value: &str| url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>();
    format!(
        "bex-context://v1/{}/{}",
        encode(context.content.kind()),
        encode(&context.id)
    )
}

pub(crate) fn context_reference(context: &MessageContext) -> String {
    let mut text = format!(
        "[{}]({})",
        crate::composer::context::literal(label(context)),
        context_href(context)
    );
    // The response card is ignored by find; its following comment is prose.
    if let ContextContent::Citation { citation } = &context.content
        && !citation.comment.is_empty()
    {
        text.push_str("\n\n");
        text.push_str(&crate::composer::context::literal(&citation.comment));
    }
    text
}

fn file_action(label: &str, path: &str) -> ContextLink {
    ContextLink {
        label: label.into(),
        target: ContextTarget::File { path: path.into() },
    }
}

fn web_action(url: &str) -> Option<ContextLink> {
    super::web_links::web_link_target(url, super::web_links::WebLinkTarget::SystemBrowser, false)?;
    Some(ContextLink {
        label: "Open link".into(),
        target: ContextTarget::Web { url: url.into() },
    })
}

fn page_title<'a>(title: Option<&'a str>, url: &'a str) -> &'a str {
    title
        .map(crate::text::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or(url)
}

fn element_identity(
    selector: Option<&str>,
    tag: &str,
    file: Option<&str>,
    line: Option<u64>,
) -> Vec<ContextSection> {
    let mut sections = vec![ContextSection::text(
        "",
        selector
            .filter(|value| !value.is_empty())
            .unwrap_or(&format!("<{tag}>")),
    )];
    if let Some(file) = file {
        sections.push(ContextSection::text(
            "",
            &line.map_or_else(|| file.into(), |line| format!("{file}:{line}")),
        ));
    }
    sections
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn context_chips(contexts: Vec<MessageContext>) -> Vec<ContextPresentation> {
    contexts
        .iter()
        .map(|context| context_chip(context, &contexts))
        .collect()
}

fn context_chip(context: &MessageContext, contexts: &[MessageContext]) -> ContextPresentation {
    let name = label(context);
    let mut chip = ContextPresentation {
        id: context.id.clone(),
        symbol: context_symbol(&context.content).into(),
        details: context_details(&context.content),
        comment: match &context.content {
            ContextContent::Citation { citation } => Some(citation.comment.clone()),
            _ => None,
        },
        citation: matches!(context.content, ContextContent::Citation { .. }),
        icon_data_url: match &context.content {
            ContextContent::SnapShot { source, .. } if source.validate().is_ok() => {
                source.app_icon_data_url.clone()
            }
            _ => None,
        },
        href: context_href(context),
        label: name.into(),
        kind: "unavailable".into(),
        accessibility_label: format!("Unavailable context, {name}"),
        tooltip: None,
        action: None,
        sections: Vec::new(),
        image: None,
        searchable: true,
        citation_preview: None,
    };
    let prefix = match &context.content {
        ContextContent::File { path } => {
            chip.kind = "mention".into();
            chip.action = Some(file_action(&format!("Preview {path}"), path));
            chip.tooltip = Some(path.clone());
            chip.accessibility_label = format!("Preview {path}");
            return chip;
        }
        ContextContent::Skill {
            path, description, ..
        } => {
            chip.kind = "skill".into();
            chip.sections.push(ContextSection::text(
                name,
                if description.is_empty() {
                    "No description is available for this skill."
                } else {
                    description
                },
            ));
            chip.action = (!path.is_empty()).then(|| file_action("View instructions", path));
            chip.accessibility_label = format!("Skill {name}");
            return chip;
        }
        ContextContent::Thread {
            host,
            storage_scope,
            session,
            ..
        } => {
            chip.kind = "thread".into();
            chip.action = Some(ContextLink {
                label: "Open thread".into(),
                target: ContextTarget::Thread {
                    host: host.clone(),
                    storage: storage_scope.clone(),
                    session: session.clone(),
                },
            });
            "Thread"
        }
        ContextContent::Environment { .. } => {
            chip.kind = "environment".into();
            chip.tooltip = Some("T3 Code environment".into());
            "Machine"
        }
        ContextContent::Terminal {
            terminal_label,
            line_start,
            line_end,
            text,
            ..
        } => {
            chip.kind = "terminal".into();
            chip.sections.push(ContextSection::text("", terminal_label));
            let range = if line_start == line_end {
                format!("Line {line_start}")
            } else {
                format!("Lines {line_start}–{line_end}")
            };
            chip.sections.push(ContextSection::text("", &range));
            if !text.is_empty() {
                chip.sections.push(ContextSection::code(
                    "Captured terminal output",
                    text,
                    "text",
                ));
            }
            "Terminal excerpt"
        }
        ContextContent::Element { element } => {
            chip.kind = "element".into();
            chip.sections.push(ContextSection::text(
                "",
                page_title(element.page_title.as_deref(), &element.page_url),
            ));
            chip.sections
                .push(ContextSection::text("", &element.page_url));
            chip.sections.extend(element_identity(
                element.selector.as_deref(),
                &element.tag_name,
                element
                    .source
                    .as_ref()
                    .and_then(|source| source.file_name.as_deref()),
                element.source.as_ref().and_then(|source| source.line),
            ));
            if let Some(component) = &element.component_name {
                chip.sections
                    .push(ContextSection::text("Component", component));
            }
            if !crate::text::trim(&element.html).is_empty() {
                chip.sections
                    .push(ContextSection::code("HTML", &element.html, "html"));
            }
            if !crate::text::trim(&element.styles).is_empty() {
                chip.sections
                    .push(ContextSection::code("CSS", &element.styles, "css"));
            }
            "Browser element"
        }
        ContextContent::Review { review } => {
            chip.kind = "review".into();
            chip.sections.push(ContextSection::text(
                &review.section_title,
                &review.range_label,
            ));
            chip.sections.push(ContextSection::text("", &review.text));
            chip.sections.push(ContextSection::code(
                &review.path,
                &review.diff,
                review.language.as_deref().unwrap_or("diff"),
            ));
            chip.action = Some(file_action("Open file", &review.path));
            "Review comment"
        }
        ContextContent::PullRequest { pull_request } => {
            chip.kind = "pullRequest".into();
            chip.sections.push(ContextSection::text(
                &format!("#{}", pull_request.number),
                &pull_request.title,
            ));
            chip.sections.push(ContextSection::text(
                "Branches",
                &format!(
                    "{} → {}",
                    pull_request.head_branch, pull_request.base_branch
                ),
            ));
            chip.action = web_action(&pull_request.url);
            "Pull request"
        }
        ContextContent::Preview { preview } => {
            chip.kind = "preview".into();
            chip.image =
                agent_protocol::context::screenshot_path(contexts, &context.id).map(str::to_owned);
            chip.sections.push(ContextSection::text(
                "",
                page_title(preview.page_title.as_deref(), &preview.page_url),
            ));
            chip.sections.push(ContextSection::text(
                &preview.target_summary,
                &preview.comment,
            ));
            for element in &preview.elements {
                chip.sections.extend(element_identity(
                    element.selector.as_deref(),
                    &element.tag_name,
                    element
                        .source
                        .as_ref()
                        .and_then(|source| source.file_name.as_deref()),
                    element.source.as_ref().and_then(|source| source.line),
                ));
                if !crate::text::trim(&element.html).is_empty() {
                    chip.sections
                        .push(ContextSection::code("HTML", &element.html, "html"));
                }
            }
            for change in &preview.style_change_details {
                chip.sections.push(ContextSection::text(
                    &change.property,
                    &format!("{} → {}", change.previous_value, change.value),
                ));
            }
            "Preview annotation"
        }
        ContextContent::Citation { citation } => {
            chip.kind = "citation".into();
            chip.searchable = false;
            chip.citation_preview = Some(CitationPreview::new(
                &citation.text,
                &citation.prefix,
                &citation.suffix,
            ));
            chip.action = Some(ContextLink {
                label: "Go to quoted response".into(),
                target: ContextTarget::Citation {
                    citation: citation.clone(),
                },
            });
            "Quoted response"
        }
        ContextContent::Image { .. } => {
            chip.kind = "image".into();
            chip.image = context.content.image_path().map(str::to_owned);
            "Image"
        }
        ContextContent::SnapShot { source, .. } => {
            chip.kind = "snapshot".into();
            chip.image = context.content.image_path().map(str::to_owned);
            chip.sections
                .push(ContextSection::text(&source.app_name, &source.window_title));
            if let Some(details) = super::snapshot::snapshot_accessibility_details(source.clone()) {
                let language = match details.format {
                    super::snapshot::SnapShotDetailsFormat::Text => "text",
                    super::snapshot::SnapShotDetailsFormat::Json => "json",
                };
                chip.sections.push(ContextSection::code(
                    "Accessibility",
                    &details.content,
                    language,
                ));
            }
            "SnapShot"
        }
        ContextContent::Model { model, .. } => {
            chip.kind = "model".into();
            chip.tooltip = Some(model.clone());
            "Model"
        }
        ContextContent::Summary { .. } => {
            chip.tooltip = Some("This context is no longer available.".into());
            "Unavailable context"
        }
    };
    chip.accessibility_label = format!("{prefix}, {name}");
    chip
}

fn context_symbol(content: &ContextContent) -> &'static str {
    use ContextContent::*;
    match content {
        Image { .. } => "photo",
        SnapShot { .. } => "camera.viewfinder",
        Citation { .. } => "quote.bubble",
        File { .. } => "doc",
        Skill { .. } => "sparkles",
        Thread { .. } => "bubble.left.and.bubble.right",
        Environment { .. } => "desktopcomputer",
        Model { .. } => "cpu",
        Terminal { .. } => "terminal",
        Element { .. } => "cursorarrow.click",
        Review { .. } => "text.bubble",
        Preview { .. } => "viewfinder",
        PullRequest { .. } => "arrow.triangle.pull",
        Summary { .. } => "ellipsis",
    }
}

/// Literal detail text for native popovers; structured sections retain code languages for TSX.
fn context_details(content: &ContextContent) -> String {
    use ContextContent::*;
    match content {
        Image { path } | File { path } => path.clone(),
        SnapShot { source, .. } => [
            Some(if source.window_title.trim().is_empty() {
                source.app_name.clone()
            } else {
                format!("{} — {}", source.app_name, source.window_title)
            }),
            super::snapshot::snapshot_accessibility_details(source.clone()).map(|details| {
                format!(
                    "{}{}",
                    details.content,
                    if details.truncated { "\n…" } else { "" }
                )
            }),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n\n"),
        Citation { citation } => citation.text.clone(),
        Terminal {
            terminal_label,
            line_start,
            line_end,
            text,
            ..
        } => format!("{terminal_label} ({line_start}–{line_end})\n\n{text}"),
        Element { element } => format!("{}\n\n{}", element.html, element.styles),
        Review { review } => format!(
            "{} ({})\n\n{}\n\n{}",
            review.path, review.range_label, review.text, review.diff
        ),
        Preview { preview } => format!(
            "{}\n\n{}\n\n{}",
            preview.page_url, preview.target_summary, preview.comment
        ),
        PullRequest { pull_request } => format!(
            "#{} {}\n{} → {}\n{}",
            pull_request.number,
            pull_request.title,
            pull_request.head_branch,
            pull_request.base_branch,
            pull_request.url
        ),
        Skill {
            path, description, ..
        } => format!("{path}\n\n{description}"),
        Thread { title, .. } => title.clone(),
        Environment { host } => host.clone(),
        Model { model, .. } => model.clone(),
        Summary { .. } => String::new(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AuthorPresentation {
    pub label: String,
    pub symbol: String,
    /// Authenticated user-message footer navigation on the current Host.
    pub action: Option<MessageAuthorAction>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum MessageAuthorAction {
    OpenThread {
        session: agent_protocol::session::SessionRef,
    },
    OpenAutomation {
        id: uuid::Uuid,
    },
}

/// Automation attribution belongs to its original conversation. Forks keep the
/// attribution but not the definition, so a foreign or deleted ID is unavailable.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn message_author_automation(
    session: agent_protocol::session::SessionRef,
    id: uuid::Uuid,
    automations: Vec<agent_protocol::automations::Automation>,
) -> Option<agent_protocol::automations::Definition> {
    automations
        .into_iter()
        .map(|value| value.definition)
        .find(|definition| definition.id == id && definition.session == session)
}

/// T3's compact user-message treatment applies only to authenticated agent authors.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn message_is_agent_authored(author: Option<Author>) -> bool {
    matches!(author, Some(Author::Agent { .. }))
}

/// The compact agent header keeps the first readable line, including authored Markdown.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn message_compact_summary(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_owned()
}

/// Human messages need no extra badge. Canonical authors carry identifiers,
/// so absent display names use localized category labels or the exact source ID.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn message_author_presentation(
    language: crate::localization::UiLanguage,
    author: Option<Author>,
    user_message: bool,
) -> Option<AuthorPresentation> {
    let (label, symbol, action) = match author? {
        Author::Automation { id } => (
            language.text("自動実行").into(),
            "bolt.badge.clock",
            user_message.then_some(MessageAuthorAction::OpenAutomation { id }),
        ),
        Author::Agent { session } => {
            let action = (user_message && session.validate().is_ok()).then(|| {
                MessageAuthorAction::OpenThread {
                    session: session.clone(),
                }
            });
            (session.id, "person.crop.circle", action)
        }
        Author::System => (language.text("システム").into(), "gearshape", None),
    };
    Some(AuthorPresentation {
        label,
        symbol: symbol.into(),
        action,
    })
}

#[cfg(test)]
mod author_tests {
    use super::*;
    #[test]
    fn author_navigation_keeps_provider_qualified_targets() {
        use agent_protocol::session::{ProviderKind, SessionRef};
        for provider in [Some(ProviderKind::Codex), Some(ProviderKind::Claude), None] {
            let session = SessionRef::new(
                provider,
                if provider.is_some() {
                    "same-native-id".into()
                } else {
                    uuid::Uuid::from_u128(12).to_string()
                },
            )
            .unwrap();
            let author = Author::Agent {
                session: session.clone(),
            };
            let shown = message_author_presentation(
                crate::localization::UiLanguage::Japanese,
                Some(author.clone()),
                true,
            )
            .unwrap();
            assert_eq!(shown.label, session.id.clone());
            assert_eq!(shown.symbol, "person.crop.circle");
            assert_eq!(
                shown.action,
                Some(MessageAuthorAction::OpenThread {
                    session: session.clone()
                })
            );
            let response = message_author_presentation(
                crate::localization::UiLanguage::Japanese,
                Some(author),
                false,
            )
            .unwrap();
            assert_eq!(response.label, shown.label);
            assert_eq!(response.symbol, shown.symbol);
            assert_eq!(
                response.action, None,
                "the T3 sender action belongs to user-message footers"
            );
        }
    }

    #[test]
    fn author_navigation_does_not_infer_targets_from_names_or_malformed_restored_addresses() {
        use agent_protocol::session::{ProviderKind, SessionRef};
        for author in [
            Author::System,
            Author::Agent {
                session: SessionRef {
                    provider: Some(ProviderKind::Codex),
                    id: " invalid ".into(),
                },
            },
        ] {
            assert!(
                message_author_presentation(
                    crate::localization::UiLanguage::Japanese,
                    Some(author),
                    true
                )
                .unwrap()
                .action
                .is_none()
            );
        }
        assert!(
            message_author_presentation(crate::localization::UiLanguage::Japanese, None, true)
                .is_none()
        );
    }

    #[test]
    fn automation_author_action_keeps_the_exact_id_and_only_user_footer_eligibility() {
        let id = uuid::Uuid::from_u128(17);
        let author = Author::Automation { id };
        let shown = message_author_presentation(
            crate::localization::UiLanguage::Japanese,
            Some(author.clone()),
            true,
        )
        .unwrap();
        assert_eq!(shown.label, "自動実行");
        assert_eq!(shown.symbol, "bolt.badge.clock");
        assert_eq!(
            shown.action,
            Some(MessageAuthorAction::OpenAutomation { id })
        );
        assert_eq!(
            message_author_presentation(
                crate::localization::UiLanguage::Japanese,
                Some(author),
                false
            )
            .unwrap()
            .action,
            None
        );
    }

    #[test]
    fn automation_author_lookup_requires_exact_id_and_conversation_without_title_fallback() {
        use agent_protocol::{
            automations::{Automation, Definition, Schedule},
            session::{ProviderKind, SessionRef},
        };
        let session = SessionRef::new(ProviderKind::Codex, "same-native-id".into()).unwrap();
        let foreign = SessionRef::new(ProviderKind::Claude, "same-native-id".into()).unwrap();
        let id = uuid::Uuid::from_u128(17);
        let definition = Definition {
            id,
            session: session.clone(),
            title: "Daily review".into(),
            prompt: "Review changes".into(),
            model: None,
            effort: None,
            service_tier: None,
            schedule: Schedule::Interval { every_ms: 60_000 },
            enabled: true,
        };
        let saved = Automation {
            definition: definition.clone(),
            next_at_ms: Some(60_000),
            last_run: None,
        };
        assert_eq!(
            message_author_automation(session.clone(), id, vec![saved.clone()]),
            Some(definition)
        );
        assert_eq!(
            message_author_automation(foreign, id, vec![saved.clone()]),
            None,
            "forked or foreign attribution must not open another conversation's definition"
        );
        assert_eq!(
            message_author_automation(
                session.clone(),
                uuid::Uuid::from_u128(18),
                vec![saved.clone()]
            ),
            None,
            "a matching title is not a matching ID"
        );
        assert_eq!(
            message_author_automation(session.clone(), id, vec![]),
            None,
            "deleted definitions are unavailable"
        );
        let fork = SessionRef::new(None, uuid::Uuid::from_u128(29).to_string()).unwrap();
        assert_eq!(
            message_author_automation(fork, id, vec![saved.clone()]),
            None
        );
        let mut foreign_saved = saved.clone();
        foreign_saved.definition.session =
            SessionRef::new(ProviderKind::Claude, "same-native-id".into()).unwrap();
        assert_eq!(
            message_author_automation(session.clone(), id, vec![foreign_saved, saved.clone()]),
            Some(saved.definition.clone())
        );
        let mut renamed = saved;
        renamed.definition.title = "Renamed after execution".into();
        assert_eq!(
            message_author_automation(session, id, vec![renamed.clone()]),
            Some(renamed.definition)
        );
    }

    #[test]
    fn compact_summary_uses_first_nonempty_line_without_losing_authored_marks() {
        assert_eq!(
            message_compact_summary(" \r\n  **Review** [file](a.rs)  \r\nnext"),
            "**Review** [file](a.rs)"
        );
        assert_eq!(
            message_compact_summary("\n 日本語の依頼 \n次の行"),
            "日本語の依頼"
        );
        assert_eq!(message_compact_summary(" \r\n\t"), "");
    }

    #[test]
    fn compact_attribution_uses_the_authenticated_author_category() {
        let session = agent_protocol::session::SessionRef::new(
            agent_protocol::session::ProviderKind::Codex,
            "agent".into(),
        )
        .unwrap();
        assert!(message_is_agent_authored(Some(Author::Agent { session })));
        assert!(!message_is_agent_authored(Some(Author::System)));
        assert!(!message_is_agent_authored(Some(Author::Automation {
            id: uuid::Uuid::from_u128(1)
        })));
        assert!(!message_is_agent_authored(None));
    }

    #[test]
    fn snapshot_details_keep_accessibility_text_literal_and_show_truncation() {
        let shown = context_chips(vec![MessageContext {
            id: "capture".into(),
            label: "Screenshot".into(),
            content: ContextContent::SnapShot {
                image_path: "/capture.png".into(),
                source: agent_protocol::snapshot::SnapShotSource {
                    captured_at_ms: 1,
                    app_name: "Browser".into(),
                    window_title: String::new(),
                    app_identifier: None,
                    app_icon_data_url: Some("data:image/png;base64,icon".into()),
                    accessibility: Some(
                        agent_protocol::snapshot::SnapShotAccessibility::FlatText {
                            text: "<button> **literal**".into(),
                            truncated: true,
                        },
                    ),
                },
            },
        }])
        .remove(0);
        assert_eq!(shown.details, "Browser\n\n<button> **literal**\n…");
        assert_eq!(shown.symbol, "camera.viewfinder");
        assert_eq!(
            shown.icon_data_url.as_deref(),
            Some("data:image/png;base64,icon")
        );
    }

    #[test]
    fn terminal_details_are_literal_and_device_identity_is_not_exposed() {
        let shown = context_chips(vec![MessageContext {
            id: "terminal".into(),
            label: "Build".into(),
            content: ContextContent::Terminal {
                terminal_id: "t".into(),
                terminal_label: "Build".into(),
                line_start: 2,
                line_end: 4,
                text: "<untrusted> **text**".into(),
            },
        }])
        .remove(0);
        assert_eq!(shown.symbol, "terminal");
        assert_eq!(shown.details, "Build (2–4)\n\n<untrusted> **text**");
        assert_eq!(shown.comment, None);
        assert!(
            message_author_presentation(crate::localization::UiLanguage::Japanese, None, true)
                .is_none()
        );
        let shown = message_author_presentation(
            crate::localization::UiLanguage::Japanese,
            Some(Author::System),
            true,
        )
        .unwrap();
        assert_eq!(shown.label, "システム");
        assert_eq!(shown.symbol, "gearshape");
    }
}
