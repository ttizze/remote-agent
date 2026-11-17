use super::*;

#[test]
fn file_references_use_typed_metadata_while_copy_and_export_keep_the_classic_body() {
    use agent_protocol::file_metadata::{FileMetadata, FileMetadataKind, FilePathMetadata};
    let path = "/uploads/opaque";
    let source = Arc::new(models::Item::new(
        "files".into(),
        ItemStatus::Completed,
        models::ItemBody::UserMessage {
            text: Some("\u{feff}\t".into()),
            content: vec![models::MessagePart::Attachment {
                name: "report[one].PDF".into(),
                path: path.into(),
            }],
        },
    ));
    let item = RenderedItem::native(
        UiLanguage::English,
        &source,
        None,
        crate::models::TurnStatus::Completed,
        None,
        None,
        None,
    );
    let canonical = item.data.body.clone();
    let mut cache = super::super::files::FileMetadataCache::default();
    let paths = vec![path.into()];
    let (generation, _) = cache.ensure(paths.clone(), 0);
    cache.received(
        generation,
        paths,
        Some(vec![FilePathMetadata {
            path: path.into(),
            metadata: Some(FileMetadata {
                kind: FileMetadataKind::File,
                byte_length: Some(1280),
                mime_type: Some("application/pdf".into()),
            }),
        }]),
        1,
    );
    let body = item.user_message_body(&cache).unwrap();
    assert_eq!(
        body.attachments_only,
        Some(super::super::message_attachment::AttachmentKind::File)
    );
    assert_eq!(
        body.text,
        "[report\\[one\\]\\.PDF](bex-context://attachment-v1/0)"
    );
    assert_eq!(body.attachments.len(), 1);
    assert_eq!(body.attachments[0].path, path);
    assert_eq!(body.attachments[0].size_label.as_deref(), Some("1.25 KB"));
    assert_eq!(body.attachments[0].icon, "t3-file-icon-pdf");
    assert_eq!(item.data.body, canonical);
    assert!(
        canonical
            .unwrap()
            .contains("Attachment: report[one].PDF (/uploads/opaque)")
    );
}
use agent_protocol::models::{ItemBody, ItemStatus, MessagePart};

#[test]
fn a_submitted_file_keeps_its_identity_actions_and_copy_body_after_acknowledgement() {
    let draft = Arc::new(crate::state::Draft {
        attachments: vec![crate::state::Attachment {
            path: "/uploads/opaque".into(),
            name: "資料 #%.pdf".into(),
            is_image: false,
        }],
        ..Default::default()
    });
    let submission = Arc::new(PendingSubmission {
        new_draft_slot: None,
        cursor: None,
        sequence: 1,
        submitted_at_ms: None,
        draft_key: crate::state::DraftKey::from("file"),
        draft,
        turn_id: None,
        after_item_id: None,
        accepted: false,
        delivery_unknown: false,
        queued: false,
    });
    let pending = RenderedItem::pending(UiLanguage::English, "pending-file", &submission, None);
    let source = Arc::new(models::Item::new(
        "ack".into(),
        ItemStatus::Completed,
        ItemBody::UserMessage {
            text: Some(String::new()),
            content: vec![MessagePart::Attachment {
                name: "資料 #%.pdf".into(),
                path: "/uploads/opaque".into(),
            }],
        },
    ));
    let accepted = RenderedItem::native(
        UiLanguage::English,
        &source,
        None,
        crate::models::TurnStatus::Completed,
        None,
        None,
        None,
    );
    for item in [&pending, &accepted] {
        let body = item.user_message_body(&Default::default()).unwrap();
        assert_eq!(
            body.attachments_only,
            Some(super::super::message_attachment::AttachmentKind::File)
        );
        assert_eq!(body.attachments.len(), 1);
        let file = &body.attachments[0];
        assert_eq!(file.href, "bex-context://attachment-v1/0");
        assert_eq!(file.name, "資料 #%.pdf");
        assert_eq!(file.path, "/uploads/opaque");
        assert!(
            file.available,
            "pending message means uploaded bytes awaiting message acknowledgement, not an unfinished upload"
        );
        assert!(file.size_label.is_none());
        assert!(body.images.is_empty());
    }
    assert_eq!(pending.data.body, accepted.data.body);
    assert_eq!(
        pending.data.body.as_deref(),
        Some("Attachment: 資料 #%.pdf (/uploads/opaque)")
    );
}

#[test]
fn specialized_context_body_preserves_original_text_chunks_and_raw_image_occurrences() {
    let context = agent_protocol::context::MessageContext {
        id: "image".into(),
        label: "Captured image".into(),
        content: agent_protocol::context::ContextContent::Image {
            path: "/tmp/captured.png".into(),
        },
    };
    let source = Arc::new(models::Item::new(
        "user".into(),
        ItemStatus::Completed,
        ItemBody::UserMessage {
            text: None,
            content: vec![
                MessagePart::Text {
                    text: "first".into(),
                },
                MessagePart::Text {
                    text: "second".into(),
                },
                MessagePart::Image {
                    source: "data:image/png;base64,original".into(),
                },
                MessagePart::Image {
                    source: "data:image/png;base64,original".into(),
                },
                MessagePart::Context {
                    context: Box::new(context),
                },
            ],
        },
    ));
    let item = RenderedItem::native(
        UiLanguage::English,
        &source,
        None,
        crate::models::TurnStatus::Completed,
        None,
        None,
        None,
    );
    let body = item.user_message_body(&Default::default()).unwrap();
    assert_eq!(
        body.text,
        "firstsecond\n\n[Captured image](bex-context://v1/image/image)"
    );
    assert_eq!(
        body.images,
        [
            "data:image/png;base64,original",
            "data:image/png;base64,original"
        ]
    );
    assert_eq!(
        item.data.image_sources,
        [
            "data:image/png;base64,original",
            "data:image/png;base64,original",
            "/tmp/captured.png"
        ]
    );
    assert_eq!(body.contexts[0].image.as_deref(), Some("/tmp/captured.png"));
    assert!(
        !item.data.body.as_ref().unwrap().contains("bex-context://"),
        "plain copy/export retain readable context"
    );
}

#[test]
fn pending_context_chips_survive_acknowledgement_without_changing_the_prompt_or_gallery() {
    let context = agent_protocol::context::MessageContext {
        id: "image".into(),
        label: "Captured image".into(),
        content: agent_protocol::context::ContextContent::Image {
            path: "/tmp/captured.png".into(),
        },
    };
    let draft = Arc::new(crate::state::Draft {
        text: "Prompt  \n".into(),
        contexts: vec![context.clone()],
        attachments: vec![crate::state::Attachment {
            path: "/tmp/raw.png".into(),
            name: "raw.png".into(),
            is_image: true,
        }],
        ..Default::default()
    });
    let pending = Arc::new(PendingSubmission {
        new_draft_slot: None,
        cursor: None,
        sequence: 1,
        submitted_at_ms: None,
        draft_key: crate::state::DraftKey::from("pending-context"),
        draft,
        turn_id: None,
        after_item_id: None,
        accepted: false,
        delivery_unknown: false,
        queued: false,
    });
    let pending = RenderedItem::pending(UiLanguage::English, "pending", &pending, None);
    let source = Arc::new(models::Item::new(
        "ack".into(),
        ItemStatus::Completed,
        ItemBody::UserMessage {
            text: Some("Prompt  \n".into()),
            content: vec![
                MessagePart::Image {
                    source: "/tmp/raw.png".into(),
                },
                MessagePart::Context {
                    context: Box::new(context),
                },
            ],
        },
    ));
    let accepted = RenderedItem::native(
        UiLanguage::English,
        &source,
        None,
        crate::models::TurnStatus::Completed,
        None,
        None,
        None,
    );
    for item in [&pending, &accepted] {
        let body = item.user_message_body(&Default::default()).unwrap();
        assert_eq!(
            body.text,
            "Prompt  \n\n\n[Captured image](bex-context://v1/image/image)"
        );
        assert_eq!(body.images, ["/tmp/raw.png"]);
        assert_eq!(
            item.data.image_sources,
            ["/tmp/raw.png", "/tmp/captured.png"]
        );
        assert_eq!(body.contexts[0].image.as_deref(), Some("/tmp/captured.png"));
    }
    assert_eq!(
        pending.data.body, accepted.data.body,
        "plain copy/export keep the same fallback across acknowledgement"
    );
}
