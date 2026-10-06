use super::*;
use crate::sync::fixtures::thread_state;
use agent_domain::{Json, ThreadId, shell};
use rstest::rstest;
use serde_json::json;

fn review(file_path: &str, range_label: &str) -> Value {
    json!({
        "version": 1,
        "kind": "review-comment",
        "contextId": "review-comment_review-1",
        "label": "a.ts",
        "sectionId": "file:src/a.ts",
        "sectionTitle": "File comment",
        "filePath": file_path,
        "startIndex": 0,
        "endIndex": 0,
        "rangeLabel": range_label,
        "text": "",
        "diff": "",
    })
}

#[rstest]
#[case("+181", "a.ts L181")]
#[case("+181 to +183", "a.ts L181 to L183")]
#[case("-63", "a.ts L63 (before)")]
#[case("L4", "a.ts L4")]
fn presents_review_range_consistently(#[case] range_label: &str, #[case] expected: &str) {
    assert_eq!(
        review_comment_label(&review("src/a.ts", range_label)),
        expected
    );
}

fn pull_request_summary() -> Value {
    json!({
        "version": 1,
        "kind": "review-comment",
        "contextId": "review-comment_review-1",
        "label": "#42",
        "sectionId": "pull-request:42",
        "sectionTitle": "PR #42",
        "filePath": "PR #42",
        "startIndex": 0,
        "endIndex": 0,
        "rangeLabel": "Improve context chips",
        "text": "Pull request details",
        "diff": "",
        "pullRequest": {
            "number": 42,
            "title": "Improve context chips",
            "url": "https://github.com/example/app/pull/42",
            "headBranch": "feat/context-chips",
            "baseBranch": "main",
            "state": "open",
            "isDraft": false,
        },
    })
}

#[test]
fn distinguishes_a_pr_summary_from_a_comment_on_its_diff() {
    let summary = pull_request_summary();
    assert!(is_pull_request_summary(&summary));
    assert_eq!(review_comment_label(&summary), "#42");
    assert_eq!(
        pull_request_display_state(&summary).as_deref(),
        Some("open")
    );
    assert_eq!(pull_request_kind_label(&summary), "Open pull request");
    let mut draft = summary.clone();
    draft["pullRequest"]["isDraft"] = json!(true);
    assert_eq!(pull_request_display_state(&draft).as_deref(), Some("draft"));
    let mut comment = summary;
    comment.as_object_mut().unwrap().remove("pullRequest");
    comment["filePath"] = json!("src/a.ts");
    comment["rangeLabel"] = json!("+12");
    comment["diff"] = json!("+const answer = 42;");
    assert!(!is_pull_request_summary(&comment));
}

#[test]
fn labels_a_preview_annotation_by_its_comment() {
    assert_eq!(
        preview_annotation_label("  Make this   bigger ", Some("Checkout")),
        "Make this bigger"
    );
    assert_eq!(
        preview_annotation_label(" ", Some(" Checkout ")),
        "Checkout"
    );
    assert_eq!(preview_annotation_label("", None), "Preview annotation");
    let long = "x".repeat(60);
    assert_eq!(
        preview_annotation_label(&long, None),
        format!("{}…", "x".repeat(47))
    );
}

#[test]
fn formats_images_with_the_image_form_and_everything_else_as_a_link() {
    assert_eq!(
        format_context_reference("image", "ctx_1", "a.png"),
        "![a.png](context://v1/image/ctx_1)"
    );
    assert_eq!(
        format_context_reference("skill", "ctx_2", "$x"),
        "[$x](context://v1/skill/ctx_2)"
    );
}

#[test]
fn keeps_same_kind_raw_ids_distinct_when_one_contains_the_namespace_prefix() {
    assert_eq!(kind_scoped_context_id("image", "x"), "image_x");
    assert_eq!(kind_scoped_context_id("image", "image_x"), "image_image_x");
}

fn folded(id: &str, slug: &str) -> bool {
    id.strip_prefix(slug)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|digest| digest.len() == 16 && digest.bytes().all(|b| b.is_ascii_hexdigit()))
}

#[test]
fn keeps_ids_that_already_fit_the_grammar_and_folds_the_rest_deterministically() {
    assert_eq!(
        to_composer_context_id("file-comment-1700-1"),
        "file-comment-1700-1"
    );
    let first = to_composer_context_id("pull-request-selection:src/a.ts:4-9");
    assert!(folded(&first, "pull-request-selection-src-a-ts-4-9"));
    assert_eq!(
        to_composer_context_id("pull-request-selection:src/a.ts:4-9"),
        first
    );
    assert_ne!(
        to_composer_context_id("pull-request-selection:src/b.ts:4-9"),
        first
    );
    assert!(folded(&to_composer_context_id("::"), "ctx"));
}

#[test]
fn tells_apart_producer_ids_that_agree_past_the_slugs_truncation_point() {
    let shared = format!("pull-request-finding:{}", "a".repeat(60));
    let first = to_composer_context_id(&format!("{shared}:1"));
    let second = to_composer_context_id(&format!("{shared}:2"));
    assert_ne!(first, second);
    assert!(first.len() <= 128);
}

fn attachment(id: &str, kind: AttachmentKind, name: &str, mime_type: &str) -> Attachment {
    Attachment {
        kind,
        source: None,
        id: id.into(),
        name: name.into(),
        mime_type: mime_type.into(),
        path: String::new(),
        size: 2048,
    }
}

fn context(records: Vec<Value>) -> MessageContext {
    MessageContext {
        version: 1,
        records: records.into_iter().map(Json).collect(),
    }
}

#[test]
fn resolves_each_link_to_a_chip_in_document_order() {
    let mut live = shell(&thread_state("Renamed thread")).unwrap();
    live.id = ThreadId::new("t1").unwrap();
    let text = "Use [$review](context://v1/skill/s1) on ![shot.png](context://v1/image/i1), \
                [clip.mov](context://v1/file/f1), [Old](context://v1/thread/th1) and \
                [gone](context://v1/terminal/missing)";
    let context = context(vec![
        json!({"version": 1, "kind": "skill", "contextId": "s1", "label": "", "name": "review"}),
        json!({"version": 1, "kind": "image", "contextId": "i1", "label": "shot.png",
               "attachmentId": "img", "name": "shot.png", "mimeType": "image/png", "sizeBytes": 2048}),
        json!({"version": 1, "kind": "file", "contextId": "f1", "label": "clip.mov",
               "attachmentId": "vid", "name": "clip.mov", "mimeType": "", "sizeBytes": 3_145_728}),
        json!({"version": 1, "kind": "thread", "contextId": "th1", "label": "Old",
               "environmentId": "host", "threadId": "t1", "title": "Old"}),
    ]);
    let attachments = [
        attachment("img", AttachmentKind::Image, "shot.png", "image/png"),
        attachment("vid", AttachmentKind::File, "clip.mov", ""),
    ];
    let chips = context_chips(
        text,
        Some(&context),
        &attachments,
        &[live],
        ContextChipSurface::Message,
    );
    let rows: Vec<(ContextChipKind, &str, &str, Option<&str>)> = chips
        .iter()
        .map(|chip| {
            (
                chip.kind,
                chip.label.as_str(),
                chip.accessible_label.as_str(),
                chip.tooltip.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (
                ContextChipKind::Skill,
                "review",
                "Skill, review",
                Some("$review")
            ),
            (
                ContextChipKind::Image,
                "shot.png",
                "Image attachment, shot.png, 2 KB",
                None
            ),
            (
                ContextChipKind::Video,
                "clip.mov",
                "Video attachment, clip.mov, 3.0 MB",
                Some("clip.mov\n3.0 MB")
            ),
            (
                ContextChipKind::Thread,
                "Renamed thread",
                "Thread, Renamed thread",
                Some("Open thread")
            ),
            (
                ContextChipKind::Unresolved,
                "gone",
                "Unavailable context, gone",
                Some("This context is no longer available.")
            ),
        ]
    );
    assert_eq!(chips[0].start, 4);
    assert_eq!(chips[0].copy_markdown, "[$review](context://v1/skill/s1)");
    assert_eq!(chips[1].attachment_id.as_deref(), Some("img"));
    assert_eq!(chips[3].thread_id.as_deref(), Some("t1"));
}

#[test]
fn a_chip_whose_record_is_of_another_kind_or_lacks_its_attachment_is_unresolved() {
    let text = "[a](context://v1/terminal/c1) [b](context://v1/file/c2)";
    let context = context(vec![
        json!({"version": 1, "kind": "mention", "contextId": "c1", "label": "a", "path": "a"}),
        json!({"version": 1, "kind": "file", "contextId": "c2", "label": "b",
               "attachmentId": "absent", "name": "b", "mimeType": "text/plain", "sizeBytes": 1}),
    ]);
    let chips = context_chips(text, Some(&context), &[], &[], ContextChipSurface::Composer);
    assert!(
        chips
            .iter()
            .all(|chip| chip.kind == ContextChipKind::Unresolved)
    );
    assert_eq!(
        chips[0].tooltip.as_deref(),
        Some("This context is no longer available. Remove it or attach it again.")
    );
}

#[test]
fn presents_pull_requests_by_state_and_review_comments_with_details() {
    let text = "[#42](context://v1/review-comment/pr) [a.ts](context://v1/review-comment/rc)";
    let mut summary = pull_request_summary();
    summary["contextId"] = json!("pr");
    summary["pullRequest"]["state"] = json!("merged");
    let mut comment = review("src/a.ts", "+12");
    comment["contextId"] = json!("rc");
    let chips = context_chips(
        text,
        Some(&context(vec![summary, comment])),
        &[],
        &[],
        ContextChipSurface::Message,
    );
    assert_eq!(chips[0].kind, ContextChipKind::PullRequestMerged);
    assert_eq!(
        chips[0].accessible_label,
        "Open Merged pull request #42: Improve context chips"
    );
    assert_eq!(
        chips[0].url.as_deref(),
        Some("https://github.com/example/app/pull/42")
    );
    assert_eq!(chips[1].kind, ContextChipKind::ReviewComment);
    assert_eq!(chips[1].label, "a.ts L12");
    assert_eq!(chips[1].details, ContextChipDetails::Popover);
}

#[test]
fn a_sent_message_shows_pictures_and_unchipped_files_outside_its_prose() {
    let text = "see [report.pdf](context://v1/file/f1)";
    let context = context(vec![json!({
        "version": 1, "kind": "file", "contextId": "f1", "label": "report.pdf",
        "attachmentId": "report", "name": "report.pdf", "mimeType": "application/pdf", "sizeBytes": 10,
    })]);
    let attachments = [
        attachment(
            "report",
            AttachmentKind::File,
            "report.pdf",
            "application/pdf",
        ),
        attachment("notes", AttachmentKind::File, "notes.txt", "text/plain"),
        attachment("shot", AttachmentKind::Image, "shot.png", "image/png"),
        attachment(
            "annotation",
            AttachmentKind::Image,
            "preview-annotation-1.png",
            "image/png",
        ),
    ];
    assert_eq!(
        standalone_attachment_ids(text, Some(&context), &attachments),
        ["notes", "shot"]
    );
}

#[test]
fn marks_unavailable_links_in_the_mobile_feed() {
    let text = "a ![x.png](context://v1/image/i1) b [gone](context://v1/terminal/t1)";
    let context = context(vec![json!({
        "version": 1, "kind": "image", "contextId": "i1", "label": "x.png",
        "attachmentId": "x", "name": "x.png", "mimeType": "image/png", "sizeBytes": 1,
    })]);
    assert_eq!(
        mobile_message_markdown(text, Some(&context)),
        "a [x.png](context://v1/image/i1) b [gone (unavailable)](context://v1/terminal/t1)"
    );
}

#[rstest]
#[case(0, "1 KB")]
#[case(1025, "2 KB")]
#[case(1_572_864, "1.5 MB")]
fn formats_attachment_sizes(#[case] bytes: u64, #[case] expected: &str) {
    assert_eq!(format_attachment_size(bytes), expected);
}

#[rstest]
#[case("a.mp4", "video/mp4", true)]
#[case("a.mov", "", true)]
#[case("a.mov", "application/octet-stream", true)]
#[case("a.mp4", "application/pdf", false)]
#[case("a.txt", "", false)]
fn recognizes_videos_without_a_recorded_type(
    #[case] name: &str,
    #[case] mime_type: &str,
    #[case] video: bool,
) {
    assert_eq!(is_video(name, mime_type), video);
}
