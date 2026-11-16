use super::*;
use crate::session::ProviderKind;

fn element(html: &str) -> ContextElement {
    ContextElement {
        page_url: "http://localhost:3000/".into(),
        page_title: Some("Home".into()),
        tag_name: "button".into(),
        selector: Some("#save".into()),
        html: html.into(),
        component_name: None,
        source: Some(ContextElementSource {
            function_name: Some("Save".into()),
            file_name: Some("save.tsx".into()),
            line: Some(4),
            column: None,
        }),
        styles: "color: red".into(),
    }
}

fn contexts() -> Vec<MessageContext> {
    let session = SessionRef::new(None, uuid::Uuid::from_u128(7).to_string()).unwrap();
    let content = [
        ContextContent::Terminal {
            terminal_id: "build".into(),
            terminal_label: "Build".into(),
            line_start: 1,
            line_end: 2,
            text: "ok".into(),
        },
        ContextContent::Element {
            element: element("<button>"),
        },
        ContextContent::Preview {
            preview: PreviewContext {
                annotation_id: "note".into(),
                page_url: "http://localhost:3000/".into(),
                page_title: None,
                comment: "larger".into(),
                target_summary: "button".into(),
                style_changes: vec!["font-size: 20px".into()],
                elements: vec![element("<a>")],
                element_ids: vec!["e1".into()],
                region_count: Some(1),
                stroke_count: None,
                style_change_details: vec![ContextStyleChange {
                    target_id: "e1".into(),
                    selector: None,
                    property: "font-size".into(),
                    previous_value: "12px".into(),
                    value: "20px".into(),
                }],
                screenshot_context_id: Some("shot".into()),
            },
        },
        ContextContent::Review {
            review: ReviewContext {
                section_id: "s".into(),
                section_title: "Changes".into(),
                path: "src/a.rs".into(),
                start: 3,
                end: 4,
                range_label: "L3-4".into(),
                text: "why?".into(),
                diff: "+a".into(),
                language: Some("rust".into()),
                pull_request: Some(PullRequestContext {
                    number: 9,
                    title: "Fix".into(),
                    url: "https://example.invalid/9".into(),
                    head_branch: "fix".into(),
                    base_branch: "main".into(),
                    state: PullRequestState::Merged,
                    draft: true,
                }),
            },
        },
        ContextContent::Environment {
            host: "studio".into(),
        },
        ContextContent::File {
            path: "/project/src/a.rs".into(),
        },
        ContextContent::Skill {
            name: "review".into(),
            path: "/skills/review/SKILL.md".into(),
            description: "Review code".into(),
        },
        ContextContent::Thread {
            host: "studio".into(),
            storage_scope: "scope".into(),
            session: session.clone(),
            title: "Earlier".into(),
        },
        ContextContent::Citation {
            citation: AssistantCitation {
                source: CitationSource {
                    host: "studio".into(),
                    storage_scope: "scope".into(),
                    session,
                    turn_id: "turn".into(),
                    item_id: "answer".into(),
                },
                range: TextRange { start: 0, end: 4 },
                text: "This".into(),
                prefix: String::new(),
                suffix: " works".into(),
                comment: "really?".into(),
            },
        },
        ContextContent::Image {
            path: "/tmp/a.png".into(),
        },
        ContextContent::Model {
            provider: ProviderKind::Codex,
            model: "gpt".into(),
        },
        ContextContent::PullRequest {
            pull_request: PullRequestContext {
                number: 1,
                title: "Open".into(),
                url: "https://example.invalid/1".into(),
                head_branch: "a".into(),
                base_branch: "main".into(),
                state: PullRequestState::Open,
                draft: false,
            },
        },
    ];
    content
        .into_iter()
        .enumerate()
        .map(|(index, content)| MessageContext {
            id: format!("context-{index}"),
            label: format!("Label {index}"),
            content,
        })
        .collect()
}

const T3_KINDS: [&str; 11] = [
    "image",
    "file",
    "terminal",
    "element",
    "preview-annotation",
    "review-comment",
    "mention",
    "skill",
    "thread",
    "environment",
    "response",
];

#[test]
fn every_context_round_trips_through_its_stored_record() {
    for context in contexts() {
        let record = context.record();
        assert_eq!(
            MessageContext::from_record(&record).as_ref(),
            Some(&context),
            "{record}"
        );
    }
}

#[test]
fn records_follow_t3_base_known_shapes_and_unknown_kind_payloads() {
    for context in contexts() {
        let record = context.record();
        assert_eq!(record["version"], 1);
        assert_eq!(record["contextId"], context.id.as_str());
        assert_eq!(record["label"], context.label.as_str());
        let kind = record["kind"].as_str().unwrap();
        if T3_KINDS.contains(&kind) {
            // Required T3 fields for each known kind.
            let required: &[&str] = match kind {
                "terminal" => &[
                    "terminalId",
                    "terminalLabel",
                    "lineStart",
                    "lineEnd",
                    "text",
                ],
                "element" => &["pageUrl", "tagName", "htmlPreview", "styles"],
                "preview-annotation" => &[
                    "annotationId",
                    "pageUrl",
                    "comment",
                    "targetSummary",
                    "styleChanges",
                ],
                "review-comment" => &[
                    "sectionId",
                    "sectionTitle",
                    "filePath",
                    "startIndex",
                    "endIndex",
                    "rangeLabel",
                    "text",
                    "diff",
                ],
                "environment" => &["environmentId"],
                "mention" => &["path"],
                "skill" => &["name"],
                "thread" => &["environmentId", "threadId", "title"],
                "response" => &["citation", "itemId"],
                other => panic!("unexpected T3 kind {other}"),
            };
            for field in required {
                assert!(!record[*field].is_null(), "{kind} lacks {field}: {record}");
            }
        } else {
            // T3 UnknownContextRecord: a non-reserved kind name with a payload.
            assert!(kind.starts_with("bex-") && kind.len() <= 40, "{kind}");
            assert!(record.get("payload").is_some(), "{record}");
        }
    }
}

#[test]
fn composer_references_and_t3_attachment_records_are_not_bex_contexts() {
    for record in [
        json!({"version":1,"contextId":"m","label":"a.rs","kind":"mention","path":"/a.rs"}),
        json!({"version":1,"contextId":"s","label":"review","kind":"skill","name":"review"}),
        json!({"version":1,"contextId":"i","label":"a.png","kind":"image","attachmentId":"a",
            "name":"a.png","mimeType":"image/png","sizeBytes":1}),
        json!({"version":1,"contextId":"u","label":"later","kind":"later-kind","payload":{"x":1}}),
    ] {
        assert_eq!(MessageContext::from_record(&record), None, "{record}");
    }
}
