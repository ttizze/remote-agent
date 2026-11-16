//! T3 `OrchestrationMessageContext` records. Kinds T3 defines keep T3's shape.
//! Bex fields T3 lacks travel in `payload`: T3 ignores it on its known kinds and
//! keeps it on forward-compatible unknown kinds, whose names Bex owns.
use super::*;
use serde_json::{Value, json};

fn element(element: &ContextElement) -> Value {
    json!({
        "pageUrl": element.page_url,
        "pageTitle": element.page_title,
        "tagName": element.tag_name,
        "selector": element.selector,
        "htmlPreview": element.html,
        "componentName": element.component_name,
        "source": element.source.as_ref().map(|source| json!({
            "functionName": source.function_name,
            "fileName": source.file_name,
            "lineNumber": source.line,
            "columnNumber": source.column,
        })),
        "styles": element.styles,
    })
}

fn read_element(value: &Value) -> Option<ContextElement> {
    let text = |key| value.get(key)?.as_str().map(str::to_owned);
    Some(ContextElement {
        page_url: text("pageUrl")?,
        page_title: text("pageTitle"),
        tag_name: text("tagName")?,
        selector: text("selector"),
        html: text("htmlPreview")?,
        component_name: text("componentName"),
        source: value
            .get("source")
            .filter(|source| !source.is_null())
            .map(|source| ContextElementSource {
                function_name: source["functionName"].as_str().map(str::to_owned),
                file_name: source["fileName"].as_str().map(str::to_owned),
                line: source["lineNumber"].as_u64(),
                column: source["columnNumber"].as_u64(),
            }),
        styles: text("styles")?,
    })
}

fn pull_request(value: &PullRequestContext) -> Value {
    json!({
        "number": value.number,
        "title": value.title,
        "url": value.url,
        "headBranch": value.head_branch,
        "baseBranch": value.base_branch,
        "state": value.state,
        "isDraft": value.draft,
    })
}

fn read_pull_request(value: &Value) -> Option<PullRequestContext> {
    Some(PullRequestContext {
        number: value["number"].as_u64()?,
        title: value["title"].as_str()?.into(),
        url: value["url"].as_str()?.into(),
        head_branch: value["headBranch"].as_str()?.into(),
        base_branch: value["baseBranch"].as_str()?.into(),
        state: serde_json::from_value(value["state"].clone()).ok()?,
        draft: value["isDraft"].as_bool()?,
    })
}

impl MessageContext {
    /// The record stored on a canonical user message.
    pub fn record(&self) -> Value {
        let (kind, fields, payload) = match &self.content {
            ContextContent::Terminal {
                terminal_id,
                terminal_label,
                line_start,
                line_end,
                text,
            } => (
                "terminal",
                json!({"terminalId": terminal_id, "terminalLabel": terminal_label,
                    "lineStart": line_start, "lineEnd": line_end, "text": text}),
                false,
            ),
            ContextContent::Element { element: value } => ("element", element(value), false),
            ContextContent::Preview { preview } => (
                "preview-annotation",
                json!({
                    "annotationId": preview.annotation_id,
                    "pageUrl": preview.page_url,
                    "pageTitle": preview.page_title,
                    "comment": preview.comment,
                    "targetSummary": preview.target_summary,
                    "styleChanges": preview.style_changes,
                    "elements": preview.elements.iter().map(element).collect::<Vec<_>>(),
                    "elementIds": preview.element_ids,
                    "regionCount": preview.region_count,
                    "strokeCount": preview.stroke_count,
                    "styleChangeDetails": preview.style_change_details,
                    "screenshotContextId": preview.screenshot_context_id,
                }),
                false,
            ),
            ContextContent::Review { review } => (
                "review-comment",
                json!({
                    "sectionId": review.section_id,
                    "sectionTitle": review.section_title,
                    "filePath": review.path,
                    "startIndex": review.start,
                    "endIndex": review.end,
                    "rangeLabel": review.range_label,
                    "text": review.text,
                    "diff": review.diff,
                    "fenceLanguage": review.language,
                    "pullRequest": review.pull_request.as_ref().map(pull_request),
                }),
                false,
            ),
            ContextContent::Environment { host } => {
                ("environment", json!({"environmentId": host}), false)
            }
            ContextContent::File { path } => ("mention", json!({"path": path}), true),
            ContextContent::Skill { name, .. } => ("skill", json!({"name": name}), true),
            ContextContent::Thread {
                host,
                session,
                title,
                ..
            } => (
                "thread",
                json!({"environmentId": host, "threadId": session.application_id(), "title": title}),
                true,
            ),
            ContextContent::Citation { citation } => (
                "response",
                json!({
                    "citation": {
                        "version": 1,
                        "environmentId": citation.source.host,
                        "threadId": citation.source.session.application_id(),
                        "messageId": citation.source.item_id,
                        "text": citation.text,
                        "comment": citation.comment,
                        "start": citation.range.start,
                        "end": citation.range.end,
                        "prefix": citation.prefix,
                        "suffix": citation.suffix,
                    },
                    "itemId": citation.source.item_id,
                }),
                true,
            ),
            ContextContent::Image { .. } => ("bex-image", json!({}), true),
            ContextContent::SnapShot { .. } => ("bex-snapshot", json!({}), true),
            ContextContent::Summary { .. } => ("bex-summary", json!({}), true),
            ContextContent::Model { .. } => ("bex-model", json!({}), true),
            ContextContent::PullRequest { .. } => ("bex-pull-request", json!({}), true),
        };
        let mut record =
            json!({"version": 1, "contextId": self.id, "label": self.label, "kind": kind});
        let object = record.as_object_mut().expect("record is an object");
        for (key, value) in fields.as_object().into_iter().flatten() {
            if !value.is_null() {
                object.insert(key.clone(), value.clone());
            }
        }
        if payload {
            object.insert(
                "payload".into(),
                serde_json::to_value(&self.content).expect("context is serializable"),
            );
        }
        record
    }

    /// A Bex context from a stored record. Plain T3 mentions and skills are
    /// composer references rather than contexts, and are read by the caller.
    pub fn from_record(record: &Value) -> Option<Self> {
        let text = |key| record.get(key)?.as_str().map(str::to_owned);
        let number = |key| record.get(key)?.as_u64();
        let content = match (record.get("payload"), text("kind")?.as_str()) {
            (Some(payload), _) => serde_json::from_value(payload.clone()).ok()?,
            (None, "terminal") => ContextContent::Terminal {
                terminal_id: text("terminalId")?,
                terminal_label: text("terminalLabel")?,
                line_start: number("lineStart")?,
                line_end: number("lineEnd")?,
                text: text("text")?,
            },
            (None, "element") => ContextContent::Element {
                element: read_element(record)?,
            },
            (None, "preview-annotation") => ContextContent::Preview {
                preview: PreviewContext {
                    annotation_id: text("annotationId")?,
                    page_url: text("pageUrl")?,
                    page_title: text("pageTitle"),
                    comment: text("comment")?,
                    target_summary: text("targetSummary")?,
                    style_changes: serde_json::from_value(record["styleChanges"].clone()).ok()?,
                    elements: record
                        .get("elements")
                        .and_then(Value::as_array)
                        .map(|elements| elements.iter().map(read_element).collect())
                        .unwrap_or(Some(Vec::new()))?,
                    element_ids: serde_json::from_value(record["elementIds"].clone())
                        .unwrap_or_default(),
                    region_count: number("regionCount"),
                    stroke_count: number("strokeCount"),
                    style_change_details: serde_json::from_value(
                        record["styleChangeDetails"].clone(),
                    )
                    .unwrap_or_default(),
                    screenshot_context_id: text("screenshotContextId"),
                },
            },
            (None, "review-comment") => ContextContent::Review {
                review: ReviewContext {
                    section_id: text("sectionId")?,
                    section_title: text("sectionTitle")?,
                    path: text("filePath")?,
                    start: number("startIndex")?,
                    end: number("endIndex")?,
                    range_label: text("rangeLabel")?,
                    text: text("text")?,
                    diff: text("diff")?,
                    language: text("fenceLanguage"),
                    pull_request: record.get("pullRequest").and_then(read_pull_request),
                },
            },
            (None, "environment") => ContextContent::Environment {
                host: text("environmentId")?,
            },
            _ => return None,
        };
        Some(Self {
            id: text("contextId")?,
            label: text("label")?,
            content,
        })
    }
}

/// One stored record as composers and providers read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoredContext {
    Mention { path: String, label: String },
    Skill { name: String },
    Context(Box<MessageContext>),
}

/// The records of a T3 `OrchestrationMessageContext`. As in T3, undecodable
/// records are dropped rather than failing the message.
pub fn stored_contexts(context: Option<&Value>) -> Vec<StoredContext> {
    context
        .and_then(|context| context.get("records"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|record| {
            if let Some(context) = MessageContext::from_record(record) {
                return Some(StoredContext::Context(Box::new(context)));
            }
            let text = |key| record.get(key)?.as_str().map(str::to_owned);
            match text("kind")?.as_str() {
                "mention" => {
                    let path = text("path")?;
                    Some(StoredContext::Mention {
                        label: text("label").unwrap_or_else(|| path.clone()),
                        path,
                    })
                }
                "skill" => Some(StoredContext::Skill {
                    name: text("name")?,
                }),
                _ => None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
