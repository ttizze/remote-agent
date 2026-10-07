//! Context chips: the inline pill each `[label](context://v1/kind/id)` link
//! renders as, in the composer and in a sent message, resolved against the
//! message's context records.
//!
//! Offsets are UTF-16 code units, the unit of the native text editors.
use crate::js_text::{utf16_offset, utf16_units};
use crate::presentation::markdown::links::is_video_file_name;
use crate::view::attachments::format_attachment_size;
use agent_domain::{
    Attachment, AttachmentKind, MessageContext, ThreadShell, context_references,
    sanitize_context_label,
};
use serde_json::Value;

const HREF_PREFIX: &str = "context://v1/";
const PREVIEW_LABEL_MAX_CHARS: usize = 48;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ContextChipKind {
    Image,
    Video,
    File,
    Mention,
    Terminal,
    Element,
    PreviewAnnotation,
    ReviewComment,
    PullRequest,
    PullRequestOpen,
    PullRequestDraft,
    PullRequestMerged,
    PullRequestClosed,
    Skill,
    Thread,
    /// The record is missing, of another kind, or of a kind this build does not know.
    Unresolved,
}

/// How a chip shows more than its label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ContextChipDetails {
    None,
    Tooltip,
    Popover,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ContextChipSurface {
    Composer,
    Message,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ContextChip {
    pub context_id: String,
    /// The kind the link names.
    pub reference_kind: String,
    pub kind: ContextChipKind,
    pub label: String,
    pub accessible_label: String,
    pub tooltip: Option<String>,
    pub details: ContextChipDetails,
    /// The link source, copied when the chip is part of a selection.
    pub copy_markdown: String,
    pub start: u32,
    pub end: u32,
    pub attachment_id: Option<String>,
    pub size_label: Option<String>,
    pub path: Option<String>,
    pub thread_id: Option<String>,
    pub url: Option<String>,
}

/// `[label](context://v1/kind/id)`, or `![label](...)` for images.
pub fn format_context_reference(kind: &str, context_id: &str, label: &str) -> String {
    format!(
        "{}[{}]({HREF_PREFIX}{kind}/{context_id})",
        if kind == "image" { "!" } else { "" },
        sanitize_context_label(label, kind)
    )
}

fn context_id_grammar(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Two FNV-1a passes over UTF-16 units, forward and reversed.
fn fnv1a64(value: &str) -> String {
    let units = utf16_units(value);
    let (mut forward, mut reverse) = (0x811c_9dc5_u32, 0x9dc5_811c_u32);
    for (index, unit) in units.iter().enumerate() {
        forward = (forward ^ u32::from(*unit)).wrapping_mul(0x0100_0193);
        reverse = (reverse ^ u32::from(units[units.len() - 1 - index])).wrapping_mul(0x0100_0193);
    }
    format!("{forward:08x}{reverse:08x}")
}

/// Folds a producer id into the context id grammar, deterministically.
pub fn to_composer_context_id(producer_id: &str) -> String {
    if context_id_grammar(producer_id) {
        return producer_id.into();
    }
    let (mut slug, mut replacing) = (String::new(), false);
    for c in producer_id.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            slug.push(c);
            replacing = false;
        } else if !replacing {
            slug.push('-');
            replacing = true;
        }
    }
    let slug: String = slug.trim_matches('-').chars().take(48).collect();
    let slug = if slug.is_empty() { "ctx".into() } else { slug };
    format!("{slug}-{}", fnv1a64(producer_id))
}

/// Producer ids are always scoped by kind, even when they already start with it.
pub fn kind_scoped_context_id(kind: &str, producer_id: &str) -> String {
    to_composer_context_id(&format!("{kind}_{producer_id}"))
}

fn field<'a>(record: &'a Value, key: &str) -> &'a str {
    record.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn pull_request(record: &Value) -> Option<&Value> {
    record.get("pullRequest").filter(|value| value.is_object())
}

fn legacy_pull_request_number(file_path: &str) -> Option<u64> {
    file_path
        .strip_prefix("PR #")
        .filter(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|digits| digits.parse().ok())
}

pub fn is_pull_request_summary(record: &Value) -> bool {
    pull_request(record).is_some()
        || (field(record, "sectionId").starts_with("pull-request:")
            && field(record, "diff").trim().is_empty()
            && legacy_pull_request_number(field(record, "filePath")).is_some())
}

fn pull_request_number(record: &Value) -> Option<u64> {
    match pull_request(record) {
        Some(pull) => pull.get("number").and_then(Value::as_u64),
        None => legacy_pull_request_number(field(record, "filePath")),
    }
}

/// `open`, `draft`, `merged` or `closed`, for a record with pull request metadata.
pub fn pull_request_display_state(record: &Value) -> Option<String> {
    let pull = pull_request(record)?;
    let state = field(pull, "state");
    Some(
        if state == "open" && pull.get("isDraft") == Some(&Value::Bool(true)) {
            "draft"
        } else {
            state
        }
        .into(),
    )
}

pub fn pull_request_kind_label(record: &Value) -> String {
    match pull_request_display_state(record) {
        None => "Pull request".into(),
        Some(state) => {
            let mut chars = state.chars();
            let first = chars.next().map(|c| c.to_uppercase().collect::<String>());
            format!(
                "{}{} pull request",
                first.unwrap_or_default(),
                chars.as_str()
            )
        }
    }
}

/// `+181 to +183` reads `L181 to L183`; `-63` reads `L63 (before)`.
fn review_range_label(range: &str) -> String {
    let parse = |part: &str| {
        let sign = part.chars().next().filter(|c| matches!(c, '+' | '-'))?;
        let digits = &part[1..];
        (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
            .then(|| (sign, digits.to_owned()))
    };
    let (first, rest) = match range.split_once(" to ") {
        Some((first, last)) => (first, Some(last)),
        None => (range, None),
    };
    let Some((sign, start)) = parse(first) else {
        return range.into();
    };
    let end = match rest.map(parse) {
        None => None,
        Some(Some((end_sign, end))) if end_sign == sign => Some(end),
        Some(_) => return range.into(),
    };
    format!(
        "L{start}{}{}",
        end.map(|end| format!(" to L{end}")).unwrap_or_default(),
        if sign == '-' { " (before)" } else { "" }
    )
}

/// `#42` for a pull request summary, else `<file> <range>`.
pub fn review_comment_label(record: &Value) -> String {
    if is_pull_request_summary(record)
        && let Some(number) = pull_request_number(record)
    {
        return format!("#{number}");
    }
    let file_path = field(record, "filePath");
    let basename = file_path.rsplit(['/', '\\']).next().unwrap_or(file_path);
    format!(
        "{basename} {}",
        review_range_label(field(record, "rangeLabel"))
    )
}

/// The annotation's comment on one line, at most 48 characters, else the page title.
pub fn preview_annotation_label(comment: &str, page_title: Option<&str>) -> String {
    let comment = comment.split_whitespace().collect::<Vec<_>>().join(" ");
    if !comment.is_empty() {
        let units = utf16_units(&comment);
        return if units.len() > PREVIEW_LABEL_MAX_CHARS {
            format!(
                "{}…",
                String::from_utf16_lossy(&units[..PREVIEW_LABEL_MAX_CHARS - 1])
            )
        } else {
            comment
        };
    }
    page_title
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or("Preview annotation")
        .into()
}

/// A video by its MIME type, or by its extension when nothing recorded the type.
pub fn is_video(name: &str, mime_type: &str) -> bool {
    let mime = mime_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    if mime.starts_with("video/") {
        return true;
    }
    if !matches!(
        mime.as_str(),
        "" | "application/octet-stream" | "binary/octet-stream" | "application/unknown"
    ) {
        return false;
    }
    is_video_file_name(name)
}

fn record<'a>(context: Option<&'a MessageContext>, context_id: &str) -> Option<&'a Value> {
    context?
        .records
        .iter()
        .map(|record| &record.0)
        .find(|record| field(record, "contextId") == context_id)
}

struct Reference<'a> {
    kind: &'a str,
    context_id: &'a str,
    label: &'a str,
}

fn unresolved(reference: &Reference<'_>, surface: ContextChipSurface) -> ContextChip {
    chip(
        reference,
        ContextChipKind::Unresolved,
        reference.label,
        format!("Unavailable context, {}", reference.label),
        Some(match surface {
            ContextChipSurface::Composer => {
                "This context is no longer available. Remove it or attach it again."
            }
            ContextChipSurface::Message => "This context is no longer available.",
        }),
        ContextChipDetails::Tooltip,
    )
}

fn chip(
    reference: &Reference<'_>,
    kind: ContextChipKind,
    label: &str,
    accessible_label: String,
    tooltip: Option<&str>,
    details: ContextChipDetails,
) -> ContextChip {
    ContextChip {
        context_id: reference.context_id.into(),
        reference_kind: reference.kind.into(),
        kind,
        label: label.into(),
        accessible_label,
        tooltip: tooltip.map(Into::into),
        details,
        copy_markdown: format_context_reference(
            reference.kind,
            reference.context_id,
            reference.label,
        ),
        start: 0,
        end: 0,
        attachment_id: None,
        size_label: None,
        path: None,
        thread_id: None,
        url: None,
    }
}

fn attachment_chip(
    reference: &Reference<'_>,
    record: &Value,
    attachments: &[Attachment],
    surface: ContextChipSurface,
) -> Option<ContextChip> {
    let image = reference.kind == "image";
    let attachment_id = field(record, "attachmentId");
    let attachment = attachments.iter().find(|attachment| {
        attachment.id == attachment_id && (attachment.kind == AttachmentKind::Image) == image
    })?;
    let name = field(record, "name");
    let size = format_attachment_size(record.get("sizeBytes").and_then(Value::as_u64)?);
    let tooltip = format!("{name}\n{size}");
    let (kind, accessible_label, tooltip) = if image {
        (
            ContextChipKind::Image,
            format!("Image attachment, {name}, {size}"),
            (surface == ContextChipSurface::Composer).then_some(tooltip.as_str()),
        )
    } else {
        let video = is_video(&attachment.name, &attachment.mime_type);
        let noun = match (video, surface) {
            (false, _) => "File",
            (true, ContextChipSurface::Composer) => "Preview video",
            (true, ContextChipSurface::Message) => "Video",
        };
        (
            if video {
                ContextChipKind::Video
            } else {
                ContextChipKind::File
            },
            format!("{noun} attachment, {name}, {size}"),
            Some(tooltip.as_str()),
        )
    };
    Some(ContextChip {
        attachment_id: Some(attachment.id.clone()),
        size_label: Some(size.clone()),
        ..chip(
            reference,
            kind,
            name,
            accessible_label,
            tooltip,
            ContextChipDetails::Tooltip,
        )
    })
}

fn review_comment_chip(reference: &Reference<'_>, record: &Value) -> ContextChip {
    let label = review_comment_label(record);
    let summary = is_pull_request_summary(record);
    let kind_label = if summary {
        pull_request_kind_label(record)
    } else {
        "Review comment".into()
    };
    let kind = if summary {
        match pull_request_display_state(record).as_deref() {
            Some("open") => ContextChipKind::PullRequestOpen,
            Some("draft") => ContextChipKind::PullRequestDraft,
            Some("merged") => ContextChipKind::PullRequestMerged,
            Some("closed") => ContextChipKind::PullRequestClosed,
            _ => ContextChipKind::PullRequest,
        }
    } else {
        ContextChipKind::ReviewComment
    };
    match pull_request(record).filter(|_| summary) {
        Some(pull) => ContextChip {
            url: Some(field(pull, "url").into()),
            ..chip(
                reference,
                kind,
                &label,
                format!("Open {kind_label} {label}: {}", field(pull, "title")),
                None,
                ContextChipDetails::None,
            )
        },
        None => chip(
            reference,
            kind,
            &label,
            format!(
                "{kind_label}, {label}{}",
                pull_request(record)
                    .map(|pull| format!(", {}", field(pull, "title")))
                    .unwrap_or_default()
            ),
            None,
            ContextChipDetails::Popover,
        ),
    }
}

fn resolve_chip(
    reference: &Reference<'_>,
    record: Option<&Value>,
    attachments: &[Attachment],
    threads: &[ThreadShell],
    surface: ContextChipSurface,
) -> ContextChip {
    let Some(record) = record.filter(|record| {
        field(record, "kind") == reference.kind && record.get("payload").is_none()
    }) else {
        return unresolved(reference, surface);
    };
    let label = field(record, "label");
    let resolved = match reference.kind {
        "mention" => {
            let path = field(record, "path");
            Some(ContextChip {
                path: Some(path.into()),
                ..chip(
                    reference,
                    ContextChipKind::Mention,
                    label,
                    format!("Preview {path}"),
                    Some(path),
                    ContextChipDetails::Tooltip,
                )
            })
        }
        "skill" => {
            let name = field(record, "name");
            let label = if label.is_empty() { name } else { label };
            Some(chip(
                reference,
                ContextChipKind::Skill,
                label,
                format!("Skill, {label}"),
                Some(&format!("${name}")),
                ContextChipDetails::Tooltip,
            ))
        }
        "thread" => {
            let thread_id = field(record, "threadId");
            let live = threads.iter().find(|shell| shell.id.as_str() == thread_id);
            let title = live
                .map(|shell| shell.title.trim())
                .filter(|title| !title.is_empty())
                .unwrap_or(field(record, "title"));
            Some(ContextChip {
                thread_id: Some(thread_id.into()),
                ..chip(
                    reference,
                    ContextChipKind::Thread,
                    title,
                    format!("Thread, {title}"),
                    Some(if live.is_some() {
                        "Open thread"
                    } else {
                        "Thread no longer available"
                    }),
                    ContextChipDetails::Tooltip,
                )
            })
        }
        "image" | "file" => attachment_chip(reference, record, attachments, surface),
        "terminal" => Some(chip(
            reference,
            ContextChipKind::Terminal,
            label,
            format!("Terminal excerpt, {label}"),
            None,
            if field(record, "text").is_empty() {
                ContextChipDetails::None
            } else {
                ContextChipDetails::Popover
            },
        )),
        "element" => Some(chip(
            reference,
            ContextChipKind::Element,
            label,
            format!("Browser element, {label}"),
            None,
            ContextChipDetails::Popover,
        )),
        "review-comment" => Some(review_comment_chip(reference, record)),
        "preview-annotation" => Some(chip(
            reference,
            ContextChipKind::PreviewAnnotation,
            label,
            format!("Preview annotation, {label}"),
            None,
            ContextChipDetails::Popover,
        )),
        _ => None,
    };
    resolved.unwrap_or_else(|| unresolved(reference, surface))
}

/// One chip per context link in `text`, in document order. `attachments` are
/// the message's (or the draft's) attachments; `threads` give live titles.
pub fn context_chips(
    text: &str,
    context: Option<&MessageContext>,
    attachments: &[Attachment],
    threads: &[ThreadShell],
    surface: ContextChipSurface,
) -> Vec<ContextChip> {
    context_references(text)
        .iter()
        .map(|occurrence| {
            let reference = Reference {
                kind: &occurrence.kind,
                context_id: &occurrence.context_id,
                label: &occurrence.label,
            };
            ContextChip {
                start: utf16_offset(text, occurrence.start) as u32,
                end: utf16_offset(text, occurrence.end) as u32,
                ..resolve_chip(
                    &reference,
                    record(context, &occurrence.context_id),
                    attachments,
                    threads,
                    surface,
                )
            }
        })
        .collect()
}

/// The attachments a sent message shows outside its prose. A file with a chip
/// needs no row; pictures always show, except annotation screenshots, which
/// their annotation shows.
pub fn standalone_attachment_ids(
    text: &str,
    context: Option<&MessageContext>,
    attachments: &[Attachment],
) -> Vec<String> {
    let chipped: Vec<&str> = context_references(text)
        .iter()
        .filter_map(|occurrence| record(context, &occurrence.context_id))
        .filter(|record| matches!(field(record, "kind"), "image" | "file"))
        .map(|record| field(record, "attachmentId"))
        .collect();
    attachments
        .iter()
        .filter(|attachment| match attachment.kind {
            AttachmentKind::Image => !attachment.name.starts_with("preview-annotation-"),
            AttachmentKind::File => !chipped.contains(&attachment.id.as_str()),
        })
        .map(|attachment| attachment.id.clone())
        .collect()
}

/// A sent message's Markdown for the mobile feed: each context link keeps its
/// label, marked when its record is missing.
pub fn mobile_message_markdown(text: &str, context: Option<&MessageContext>) -> String {
    let mut result = String::new();
    let mut cursor = 0;
    for occurrence in context_references(text) {
        let available = record(context, &occurrence.context_id).is_some();
        result.push_str(&text[cursor..occurrence.start]);
        result.push_str(&format!(
            "[{}{}]({HREF_PREFIX}{}/{})",
            occurrence.label,
            if available { "" } else { " (unavailable)" },
            occurrence.kind,
            occurrence.context_id
        ));
        cursor = occurrence.end;
    }
    result.push_str(&text[cursor..]);
    result
}

#[cfg(test)]
mod tests;
