//! Structured user context survives drafts, queues and transcript storage. The
//! provider receives its contents as data; it does not grant resource access.
use crate::{
    ids::{ItemId, TurnId},
    session::{ProviderKind, SessionRef},
};
use serde::{Deserialize, Serialize};

pub const CITATION_TEXT_LIMIT: usize = 8_000;
pub const CITATION_CONTEXT_LIMIT: usize = 32;
const CONTEXT_START: &str = "\n<user-context-data>\n";
const CONTEXT_END: &str = "\n</user-context-data>\n";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CitationSource {
    pub host: String,
    pub storage_scope: String,
    pub session: SessionRef,
    pub turn_id: TurnId,
    pub item_id: ItemId,
}

/// Native text controls address rendered text in UTF-16 code units. A boundary
/// between the two code units of a surrogate pair is never a valid selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextRange {
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantCitation {
    pub source: CitationSource,
    pub range: TextRange,
    pub text: String,
    pub prefix: String,
    pub suffix: String,
    pub comment: String,
}

fn byte_offset(text: &str, units: u32) -> Option<usize> {
    let mut offset = 0u32;
    for (byte, character) in text.char_indices() {
        if offset == units {
            return Some(byte);
        }
        offset = offset.checked_add(character.len_utf16() as u32)?;
        if offset > units {
            return None;
        }
    }
    (offset == units).then_some(text.len())
}

pub(crate) fn bounded(text: &str, limit: usize) -> bool {
    text.encode_utf16().take(limit + 1).count() <= limit
}
fn identifier(value: &str) -> bool {
    !value.trim().is_empty() && bounded(value, 512)
}
fn snapshot_path(value: &str) -> bool {
    crate::paths::is_absolute_host_path(value)
        && !crate::paths::is_network_host_path(value)
        && bounded(value, 2_048)
}

impl AssistantCitation {
    pub fn from_selection(
        source: CitationSource,
        body: &str,
        range: TextRange,
    ) -> Result<Self, &'static str> {
        if range.start >= range.end {
            return Err("citation selection is empty");
        }
        let start = byte_offset(body, range.start).ok_or("invalid citation start")?;
        let end = byte_offset(body, range.end).ok_or("invalid citation end")?;
        let mut prefix: Vec<char> = Vec::new();
        let mut units = 0;
        for character in body[..start].chars().rev() {
            units += character.len_utf16();
            if units > CITATION_CONTEXT_LIMIT {
                break;
            }
            prefix.push(character);
        }
        let mut suffix = String::new();
        units = 0;
        for character in body[end..].chars() {
            units += character.len_utf16();
            if units > CITATION_CONTEXT_LIMIT {
                break;
            }
            suffix.push(character);
        }
        let citation = Self {
            source,
            range,
            text: body[start..end].into(),
            prefix: prefix.into_iter().rev().collect(),
            suffix,
            comment: String::new(),
        };
        citation.validate()?;
        Ok(citation)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        let source = &self.source;
        source.session.validate()?;
        if ![
            source.host.as_str(),
            source.storage_scope.as_str(),
            source.turn_id.as_str(),
            source.item_id.as_str(),
        ]
        .into_iter()
        .all(identifier)
        {
            return Err("citation source is invalid");
        }
        let units = self
            .text
            .encode_utf16()
            .take(CITATION_TEXT_LIMIT + 1)
            .count();
        if units > CITATION_TEXT_LIMIT || !bounded(&self.comment, CITATION_TEXT_LIMIT) {
            return Err("citation text or comment is too long");
        }
        if self.range.start >= self.range.end
            || self.text.trim().is_empty()
            || self.range.end - self.range.start != units as u32
        {
            return Err("citation selection does not match its range");
        }
        if !bounded(&self.prefix, CITATION_CONTEXT_LIMIT)
            || !bounded(&self.suffix, CITATION_CONTEXT_LIMIT)
        {
            return Err("citation surrounding context is too long");
        }
        Ok(())
    }

    /// Reuse the exact range when it still matches. Otherwise relocate only a
    /// unique match with the saved surrounding context; ambiguity returns None.
    pub fn locate(&self, body: &str) -> Option<TextRange> {
        self.validate().ok()?;
        let matches = |start: usize, end: usize| {
            body.get(start..end) == Some(self.text.as_str())
                && body[..start].ends_with(&self.prefix)
                && body[end..].starts_with(&self.suffix)
        };
        if let (Some(start), Some(end)) = (
            byte_offset(body, self.range.start),
            byte_offset(body, self.range.end),
        ) && matches(start, end)
        {
            return Some(self.range);
        }
        let mut found = None;
        let mut cursor = 0;
        while let Some(relative) = body[cursor..].find(&self.text) {
            let start = cursor + relative;
            let end = start + self.text.len();
            if matches(start, end) {
                if found.is_some() {
                    return None;
                }
                found = Some(TextRange {
                    start: u32::try_from(body[..start].encode_utf16().count()).ok()?,
                    end: u32::try_from(body[..end].encode_utf16().count()).ok()?,
                });
            }
            cursor = start + body[start..].chars().next()?.len_utf8();
        }
        found
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextElementSource {
    pub function_name: Option<String>,
    pub file_name: Option<String>,
    pub line: Option<u64>,
    pub column: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextElement {
    pub page_url: String,
    pub page_title: Option<String>,
    pub tag_name: String,
    pub selector: Option<String>,
    pub html: String,
    pub component_name: Option<String>,
    pub source: Option<ContextElementSource>,
    pub styles: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextStyleChange {
    pub target_id: String,
    pub selector: Option<String>,
    pub property: String,
    pub previous_value: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewContext {
    pub annotation_id: String,
    pub page_url: String,
    pub page_title: Option<String>,
    pub comment: String,
    pub target_summary: String,
    pub style_changes: Vec<String>,
    pub elements: Vec<ContextElement>,
    pub element_ids: Vec<String>,
    pub region_count: Option<u64>,
    pub stroke_count: Option<u64>,
    pub style_change_details: Vec<ContextStyleChange>,
    pub screenshot_context_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PullRequestState {
    Open,
    Closed,
    Merged,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestContext {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub head_branch: String,
    pub base_branch: String,
    pub state: PullRequestState,
    pub draft: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewContext {
    pub section_id: String,
    pub section_title: String,
    pub path: String,
    pub start: u64,
    pub end: u64,
    pub range_label: String,
    pub text: String,
    pub diff: String,
    pub language: Option<String>,
    pub pull_request: Option<PullRequestContext>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ContextContent {
    Image {
        path: String,
    },
    SnapShot {
        image_path: String,
        source: crate::snapshot::SnapShotSource,
    },
    /// Only a deferred transcript item contains this. Read the original item
    /// before editing or submitting its context again.
    Summary {
        kind: String,
    },
    Citation {
        citation: AssistantCitation,
    },
    File {
        path: String,
    },
    Skill {
        name: String,
        path: String,
        description: String,
    },
    Thread {
        host: String,
        storage_scope: String,
        session: SessionRef,
        title: String,
    },
    Environment {
        host: String,
    },
    Model {
        provider: ProviderKind,
        model: String,
    },
    Terminal {
        terminal_id: String,
        terminal_label: String,
        line_start: u64,
        line_end: u64,
        text: String,
    },
    Element {
        element: ContextElement,
    },
    Review {
        review: ReviewContext,
    },
    Preview {
        preview: PreviewContext,
    },
    PullRequest {
        pull_request: PullRequestContext,
    },
}

impl ContextContent {
    pub fn image_path(&self) -> Option<&str> {
        match self {
            Self::SnapShot { image_path, .. } | Self::Image { path: image_path }
                if snapshot_path(image_path) =>
            {
                Some(image_path)
            }
            _ => None,
        }
    }
    pub fn kind(&self) -> &str {
        match self {
            Self::Image { .. } => "image",
            Self::SnapShot { .. } => "snapshot",
            Self::Summary { kind } => kind,
            Self::Citation { .. } => "citation",
            Self::File { .. } => "file",
            Self::Skill { .. } => "skill",
            Self::Thread { .. } => "thread",
            Self::Environment { .. } => "environment",
            Self::Model { .. } => "model",
            Self::Terminal { .. } => "terminal",
            Self::Element { .. } => "element",
            Self::Review { .. } => "review",
            Self::Preview { .. } => "preview",
            Self::PullRequest { .. } => "pullRequest",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageContext {
    pub id: String,
    pub label: String,
    pub content: ContextContent,
}

impl MessageContext {
    pub fn validate(&self) -> Result<usize, &'static str> {
        if !identifier(&self.id) || !bounded(&self.label, 2_048) {
            return Err("invalid context identity or label");
        }
        match &self.content {
            ContextContent::Image { path } if !snapshot_path(path) => {
                Err("invalid context image path")
            }
            ContextContent::SnapShot { image_path, source } => {
                if !snapshot_path(image_path) {
                    return Err("invalid snapshot image path");
                }
                source.validate()
            }
            ContextContent::Summary { .. } => Err("load the original context before submitting"),
            ContextContent::Citation { citation } => citation.validate(),
            ContextContent::File { path } if path.trim().is_empty() || !bounded(path, 2_048) => {
                Err("invalid context path")
            }
            ContextContent::Skill { name, path, .. }
                if !identifier(name) || path.trim().is_empty() =>
            {
                Err("invalid skill context")
            }
            ContextContent::Thread {
                host,
                storage_scope,
                session,
                ..
            } => {
                if !identifier(host) || !identifier(storage_scope) {
                    return Err("invalid thread context");
                }
                session.validate()
            }
            ContextContent::Environment { host } if !identifier(host) => {
                Err("invalid environment context")
            }
            ContextContent::Model { model, .. } if !identifier(model) => {
                Err("invalid model context")
            }
            ContextContent::Terminal {
                terminal_id,
                terminal_label,
                line_start,
                line_end,
                ..
            } if !identifier(terminal_id)
                || !identifier(terminal_label)
                || line_start > line_end =>
            {
                Err("invalid terminal context")
            }
            ContextContent::Review { review }
                if review.start > review.end || review.path.trim().is_empty() =>
            {
                Err("invalid review context")
            }
            ContextContent::PullRequest { pull_request } if pull_request.number == 0 => {
                Err("invalid pull request context")
            }
            _ => Ok(()),
        }?;
        // Validate recursive payload bounds before serializing them.
        let size = serde_json::to_vec(self)
            .map_err(|_| "invalid context")?
            .len();
        if size > 256 * 1024 {
            return Err("context is too large");
        }
        Ok(size)
    }

    /// Native providers lacking structured-context input receive a bounded data
    /// record. The original typed record remains in the canonical transcript.
    pub fn provider_text(&self) -> String {
        format!(
            "{CONTEXT_START}{}{CONTEXT_END}",
            serde_json::to_string(self).expect("context is serializable")
        )
    }
}

/// Decode the provider's text transport, including providers which merge
/// adjacent text blocks. Malformed or unrelated text stays literal user text.
fn decode_text(text: String) -> Vec<crate::items::MessagePart> {
    use crate::items::MessagePart;
    if !text.contains(CONTEXT_START) {
        return vec![MessagePart::Text { text }];
    }
    let mut parts = Vec::new();
    let mut retained = 0;
    let mut cursor = 0;
    while let Some(relative) = text[cursor..].find(CONTEXT_START) {
        let start = cursor + relative;
        let body_start = start + CONTEXT_START.len();
        let mut values =
            serde_json::Deserializer::from_str(&text[body_start..]).into_iter::<MessageContext>();
        if let Some(Ok(context)) = values.next() {
            let end = body_start + values.byte_offset();
            if context.validate().is_ok() && text[end..].starts_with(CONTEXT_END) {
                if start > retained {
                    parts.push(MessagePart::Text {
                        text: text[retained..start].into(),
                    });
                }
                parts.push(MessagePart::Context {
                    context: Box::new(context),
                });
                retained = end + CONTEXT_END.len();
                cursor = retained;
                continue;
            }
        }
        cursor = body_start;
    }
    if parts.is_empty() {
        return vec![MessagePart::Text { text }];
    }
    if retained < text.len() {
        parts.push(MessagePart::Text {
            text: text[retained..].into(),
        });
    }
    parts
}

pub fn user_message(
    text: Option<String>,
    content: Vec<crate::items::MessagePart>,
) -> crate::items::ItemBody {
    use crate::items::{ItemBody, MessagePart};
    let mut content: Vec<_> = content
        .into_iter()
        .flat_map(|part| match part {
            MessagePart::Text { text } => decode_text(text),
            _ => vec![part],
        })
        .collect();
    if content
        .iter()
        .any(|part| matches!(part, MessagePart::Context { .. }))
    {
        if !content
            .iter()
            .any(|part| matches!(part, MessagePart::Text { .. }))
            && let Some(text) = text.filter(|text| !text.is_empty())
        {
            let decoded = decode_text(text).into_iter().filter(|part| match part {
                    MessagePart::Context { context } => !content.iter().any(|part| matches!(part, MessagePart::Context {context: other} if other.id == context.id)),
                    _ => true,
                }).collect::<Vec<_>>();
            content.splice(0..0, decoded);
        }
        return ItemBody::UserMessage {
            text: None,
            content,
        };
    }
    if let Some(text) = text.as_ref().filter(|text| text.contains(CONTEXT_START)) {
        let decoded = decode_text(text.clone());
        if decoded
            .iter()
            .any(|part| matches!(part, MessagePart::Context { .. }))
        {
            content.retain(|part| !matches!(part, MessagePart::Text { .. }));
            return ItemBody::UserMessage {
                text: None,
                content: decoded.into_iter().chain(content).collect(),
            };
        }
    }
    ItemBody::UserMessage { text, content }
}

/// Resolve a screenshot from the same message; a reference cannot name an
/// arbitrary file context or another reference.
pub fn screenshot_path<'a>(contexts: &'a [MessageContext], id: &str) -> Option<&'a str> {
    let context = contexts.iter().find(|context| context.id == id)?;
    let source = match &context.content {
        ContextContent::Preview { preview } => contexts
            .iter()
            .find(|context| Some(&context.id) == preview.screenshot_context_id.as_ref())?,
        _ => context,
    };
    source.content.image_path()
}

pub fn validate_contexts<'a>(
    contexts: impl IntoIterator<Item = &'a MessageContext>,
) -> Result<(), &'static str> {
    let mut ids = std::collections::BTreeMap::new();
    let mut bytes = 0;
    for context in contexts {
        if ids.insert(context.id.as_str(), context).is_some() {
            return Err("duplicate context identity");
        }
        bytes += context.validate()?;
        if ids.len() > 64 || bytes > 1024 * 1024 {
            return Err("context capacity exceeded");
        }
    }
    for context in ids.values() {
        if let ContextContent::Preview { preview } = &context.content
            && let Some(id) = &preview.screenshot_context_id
            && !ids.get(id.as_str()).is_some_and(|source| {
                matches!(
                    source.content,
                    ContextContent::SnapShot { .. } | ContextContent::Image { .. }
                )
            })
        {
            return Err("annotation screenshot must reference a screenshot in the same message");
        }
    }
    Ok(())
}

pub fn remove_contexts(contexts: &[MessageContext], ids: &[String]) -> Vec<MessageContext> {
    contexts
        .iter()
        .filter(|context| !ids.contains(&context.id))
        .cloned()
        .map(|mut context| {
            if let ContextContent::Preview { preview } = &mut context.content
                && preview
                    .screenshot_context_id
                    .as_ref()
                    .is_some_and(|id| ids.contains(id))
            {
                preview.screenshot_context_id = None;
            }
            context
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn annotation() -> MessageContext {
        MessageContext {
            id: "note".into(),
            label: "Annotation".into(),
            content: ContextContent::Preview {
                preview: PreviewContext {
                    annotation_id: "annotation".into(),
                    page_url: "https://example.test".into(),
                    page_title: None,
                    comment: "This button".into(),
                    target_summary: String::new(),
                    style_changes: vec![],
                    elements: vec![],
                    element_ids: vec![],
                    region_count: None,
                    stroke_count: None,
                    style_change_details: vec![],
                    screenshot_context_id: Some("image".into()),
                },
            },
        }
    }
    fn screenshot() -> MessageContext {
        MessageContext {
            id: "image".into(),
            label: "Screenshot".into(),
            content: ContextContent::SnapShot {
                image_path: "/capture.png".into(),
                source: crate::snapshot::SnapShotSource {
                    captured_at_ms: 1,
                    app_name: "Browser".into(),
                    window_title: String::new(),
                    app_identifier: None,
                    app_icon_data_url: None,
                    accessibility: None,
                },
            },
        }
    }
    #[test]
    fn annotation_images_resolve_within_the_message_regardless_of_context_order() {
        let contexts = vec![annotation(), screenshot()];
        validate_contexts(&contexts).unwrap();
        assert_eq!(screenshot_path(&contexts, "note"), Some("/capture.png"));
        assert!(validate_contexts([&contexts[0]]).is_err());
        let mut wrong_kind = screenshot();
        wrong_kind.content = ContextContent::File {
            path: "/capture.png".into(),
        };
        assert!(validate_contexts([&contexts[0], &wrong_kind]).is_err());
        let bytes = rmp_serde::to_vec_named(&contexts).unwrap();
        let restored: Vec<MessageContext> = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(screenshot_path(&restored, "note"), Some("/capture.png"));
        assert_eq!(screenshot_path(&contexts, "unknown"), None);
        let removed = remove_contexts(&contexts, &["image".into()]);
        assert_eq!(removed.len(), 1);
        validate_contexts(&removed).unwrap();
        assert_eq!(screenshot_path(&removed, "note"), None);
        let ContextContent::Preview { preview } = &removed[0].content else {
            panic!("keep annotation")
        };
        assert_eq!(preview.comment, "This button");
    }

    fn source() -> CitationSource {
        CitationSource {
            host: "host".into(),
            storage_scope: "scope".into(),
            session: SessionRef::new(ProviderKind::Codex, "thread".into()).unwrap(),
            turn_id: "turn".into(),
            item_id: "item".into(),
        }
    }

    #[test]
    fn citations_keep_unicode_ranges_comments_and_scope_through_storage() {
        let mut citation = AssistantCitation::from_selection(
            source(),
            "前😀**引用**後",
            TextRange { start: 1, end: 9 },
        )
        .unwrap();
        assert_eq!(citation.text, "😀**引用**");
        assert_eq!(citation.prefix, "前");
        assert_eq!(citation.suffix, "後");
        citation.comment = "この点を説明して".into();
        let encoded = rmp_serde::to_vec_named(&citation).unwrap();
        assert_eq!(
            rmp_serde::from_slice::<AssistantCitation>(&encoded).unwrap(),
            citation
        );
        assert!(
            AssistantCitation::from_selection(source(), "前😀後", TextRange { start: 2, end: 3 })
                .is_err()
        );
        assert!(
            AssistantCitation::from_selection(source(), "   ", TextRange { start: 0, end: 3 })
                .is_err()
        );
    }

    #[test]
    fn relocation_uses_surrounding_text_and_rejects_changed_or_ambiguous_sources() {
        let citation = AssistantCitation::from_selection(
            source(),
            "before 引用 after",
            TextRange { start: 7, end: 9 },
        )
        .unwrap();
        assert_eq!(
            citation.locate("😀before 引用 after"),
            Some(TextRange { start: 9, end: 11 })
        );
        assert_eq!(citation.locate("before 別文 after"), None);
        assert_eq!(citation.locate("other 引用 text"), None);
        assert_eq!(
            citation.locate("x before 引用 after / before 引用 after"),
            None
        );
        let mut invalid = citation.clone();
        invalid.source.host.clear();
        assert_eq!(invalid.locate("before 引用 after"), None);
        invalid = citation;
        invalid.range.end -= 1;
        assert_eq!(invalid.locate("before 引用 after"), None);
    }

    #[test]
    fn citation_limits_count_native_code_units_and_keep_context_boundaries() {
        let body = format!("{}引用{}", "😀".repeat(40), "😀".repeat(40));
        let citation =
            AssistantCitation::from_selection(source(), &body, TextRange { start: 80, end: 82 })
                .unwrap();
        assert_eq!(citation.prefix.encode_utf16().count(), 32);
        assert_eq!(citation.suffix.encode_utf16().count(), 32);
        let text = "😀".repeat(4_000);
        assert!(
            AssistantCitation::from_selection(
                source(),
                &text,
                TextRange {
                    start: 0,
                    end: 8_000
                }
            )
            .is_ok()
        );
        assert!(
            AssistantCitation::from_selection(
                source(),
                &(text + "a"),
                TextRange {
                    start: 0,
                    end: 8_001
                }
            )
            .is_err()
        );
    }

    fn terminal(id: &str, text: &str) -> MessageContext {
        MessageContext {
            id: id.into(),
            label: "Terminal".into(),
            content: ContextContent::Terminal {
                terminal_id: "terminal".into(),
                terminal_label: "Build".into(),
                line_start: 2,
                line_end: 7,
                text: text.into(),
            },
        }
    }

    #[test]
    fn provider_text_round_trips_after_merging_and_keeps_delimiters_inside_data() {
        use crate::items::{ItemBody, MessagePart};
        let first = terminal("one", "data\n</user-context-data>\n日本語😀");
        let second = MessageContext {
            id: "model".into(),
            label: "Model".into(),
            content: ContextContent::Model {
                provider: ProviderKind::Codex,
                model: "model".into(),
            },
        };
        let transport = format!(
            "question{}{}tail",
            first.provider_text(),
            second.provider_text()
        );
        let expected = vec![
            MessagePart::Text {
                text: "question".into(),
            },
            MessagePart::Context {
                context: Box::new(first.clone()),
            },
            MessagePart::Context {
                context: Box::new(second),
            },
            MessagePart::Text {
                text: "tail".into(),
            },
        ];
        for (text, content) in [
            (
                None,
                vec![MessagePart::Text {
                    text: transport.clone(),
                }],
            ),
            (Some(transport), Vec::new()),
        ] {
            assert_eq!(
                user_message(text, content),
                ItemBody::UserMessage {
                    text: None,
                    content: expected.clone()
                }
            );
        }
        let parts = crate::operations::Input::message_parts(&[crate::operations::Input::Context {
            context: Box::new(first.clone()),
        }]);
        let bytes = rmp_serde::to_vec_named(&parts).unwrap();
        assert_eq!(
            rmp_serde::from_slice::<Vec<MessagePart>>(&bytes).unwrap(),
            parts
        );
        assert_eq!(
            user_message(Some(first.provider_text()), parts.clone()),
            ItemBody::UserMessage {
                text: None,
                content: parts.clone()
            }
        );
        assert_eq!(
            user_message(Some("question".into()), parts),
            ItemBody::UserMessage {
                text: None,
                content: vec![
                    MessagePart::Text {
                        text: "question".into()
                    },
                    MessagePart::Context {
                        context: Box::new(first)
                    }
                ]
            }
        );
    }

    #[test]
    fn malformed_transport_and_invalid_context_never_erase_user_text() {
        use crate::items::{ItemBody, MessagePart};
        for text in [
            "literal text".into(),
            format!("{CONTEXT_START}not json{CONTEXT_END}"),
            format!("{CONTEXT_START}{{}}{CONTEXT_END}"),
            terminal("", "bad identity").provider_text(),
        ] {
            assert_eq!(
                user_message(None, vec![MessagePart::Text { text: text.clone() }]),
                ItemBody::UserMessage {
                    text: None,
                    content: vec![MessagePart::Text { text }]
                }
            );
        }
        let mut first = terminal("id", "output");
        assert!(validate_contexts([&first, &first]).is_err());
        if let ContextContent::Terminal { line_end, .. } = &mut first.content {
            *line_end = 0;
        }
        assert!(validate_contexts([&first]).is_err());
        assert!(validate_contexts([&terminal("id", &"a".repeat(256 * 1024))]).is_err());
    }

    #[test]
    fn deferred_context_keeps_its_chip_identity_and_cannot_be_sent_as_if_complete() {
        use crate::{
            execution::ItemStatus,
            items::{Item, ItemBody, MessagePart},
        };
        let context = terminal("output", &"data".repeat(30_000));
        let original = Item::new(
            "user".into(),
            ItemStatus::Completed,
            ItemBody::UserMessage {
                text: None,
                content: vec![MessagePart::Context {
                    context: Box::new(context.clone()),
                }],
            },
        );
        let mut summary = original.clone();
        summary.defer();
        let ItemBody::UserMessage { content, .. } = summary.body() else {
            panic!("user message");
        };
        let MessagePart::Context {
            context: summarized,
        } = &content[0]
        else {
            panic!("context");
        };
        assert_eq!(summarized.id, context.id);
        assert_eq!(summarized.label, context.label);
        assert_eq!(
            summarized.content,
            ContextContent::Summary {
                kind: "terminal".into()
            }
        );
        assert!(validate_contexts([summarized.as_ref()]).is_err());
        assert!(rmp_serde::to_vec_named(&summary).unwrap().len() < 1024);
        assert!(
            matches!(original.body(), ItemBody::UserMessage {content, ..} if content == &[MessagePart::Context {context: Box::new(context)}])
        );
    }

    proptest::proptest! {
        #[test]
        fn unicode_selections_round_trip_at_native_boundaries(prefix in ".{0,40}", selection in ".{1,40}", suffix in ".{0,40}") {
            proptest::prop_assume!(!selection.trim().is_empty());
            let start = prefix.encode_utf16().count() as u32;
            let range = TextRange {start, end: start + selection.encode_utf16().count() as u32};
            let body = format!("{prefix}{selection}{suffix}");
            let citation = AssistantCitation::from_selection(source(), &body, range).unwrap();
            proptest::prop_assert_eq!(&citation.text, &selection);
            proptest::prop_assert_eq!(citation.locate(&body), Some(range));
            proptest::prop_assert!(citation.prefix.encode_utf16().count() <= 32);
            proptest::prop_assert!(citation.suffix.encode_utf16().count() <= 32);
        }
    }
}
