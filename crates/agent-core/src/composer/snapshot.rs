//! Capture metadata is committed with its uploaded image in the same draft update.
use agent_protocol::{
    context::{ContextContent, MessageContext},
    snapshot::SnapShotSource,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SnapShotCapture {
    pub id: String,
    pub source: SnapShotSource,
}

impl SnapShotCapture {
    pub(crate) fn append(
        self,
        current: &[MessageContext],
        image_path: String,
        is_image: bool,
    ) -> Result<Vec<MessageContext>, &'static str> {
        if !is_image {
            return Err("Snap Shot requires an image attachment");
        }
        if current.iter().any(|existing| existing.id == self.id) {
            return Err("snapshot id is already in the draft");
        }
        let context = MessageContext {
            id: self.id,
            label: self.source.app_name.clone(),
            content: ContextContent::SnapShot {
                image_path,
                source: self.source,
            },
        };
        super::context::change(
            current,
            super::context::ContextAction::Add {
                context: Box::new(context),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{
        Attachment, Draft, DraftKey, Event, Intent, Snapshot,
        operations::{Operation, UploadAttachment},
        reduce,
    };
    use std::sync::Arc;
    fn capture() -> SnapShotCapture {
        SnapShotCapture {
            id: "capture".into(),
            source: SnapShotSource {
                captured_at_ms: 1,
                app_name: "Editor".into(),
                window_title: "Project".into(),
                app_identifier: None,
                app_icon_data_url: None,
                accessibility: Some(agent_protocol::snapshot::SnapShotAccessibility::FlatText {
                    text: "Document".into(),
                    truncated: true,
                }),
            },
        }
    }
    fn upload() -> UploadAttachment {
        UploadAttachment {
            draft_key: DraftKey::from("/project"),
            attachment: Attachment {
                path: "/client/capture.png".into(),
                name: "capture.png".into(),
                is_image: true,
            },
            snapshot: Some(capture()),
        }
    }
    #[test]
    fn image_only_capture_has_a_readable_caption_without_an_empty_title() {
        let mut capture = capture();
        capture.source.window_title.clear();
        capture.source.accessibility = None;
        let contexts = capture
            .append(&[], "/host/capture.png".into(), true)
            .unwrap();
        assert_eq!(
            crate::composer::context::display_text(&contexts[0]),
            "Editor"
        );
    }
    #[test]
    fn snapshot_upload_commits_image_and_metadata_together_and_round_trips() {
        let mut state = Snapshot::default();
        let mut operation = upload();
        operation.prepare(&mut state, "").unwrap();
        let key = operation.draft_key.clone();
        Arc::make_mut(&mut state.input.drafts).insert(
            key.clone(),
            Arc::new(Draft {
                text: "Keep this".into(),
                ..Default::default()
            }),
        );
        operation
            .complete(&mut state, "", "/host/capture.png".into(), false)
            .unwrap();
        let draft = &state.input.drafts[&key];
        assert_eq!(draft.text, "Keep this");
        assert_eq!(draft.attachments[0].path, "/host/capture.png");
        assert!(
            matches!(&draft.contexts[0].content, ContextContent::SnapShot {image_path,source} if image_path == "/host/capture.png" && source == &capture().source)
        );
        let restored =
            crate::persistence::decode(&crate::persistence::encode(&state).unwrap()).unwrap();
        assert_eq!(restored.input.drafts[&key], *draft);
        let (removed, _) = reduce(
            &restored,
            "",
            Event::Intent(Intent::RemoveAttachment {
                draft_key: key.clone(),
                index: 0,
            }),
        );
        assert!(removed.input.drafts[&key].attachments.is_empty());
        assert!(removed.input.drafts[&key].contexts.is_empty());
        assert_eq!(removed.input.drafts[&key].text, "Keep this");
    }
    #[test]
    fn invalid_or_full_metadata_leaves_the_original_draft_intact() {
        let mut state = Snapshot::default();
        let mut non_image = upload();
        non_image.attachment.is_image = false;
        assert!(non_image.prepare(&mut state, "").is_err());
        let mut invalid = upload();
        invalid.snapshot.as_mut().unwrap().source.app_name.clear();
        assert!(invalid.prepare(&mut state, "").is_err());
        let operation = upload();
        let key = operation.draft_key.clone();
        let full = Arc::new(Draft {
            text: "Keep this".into(),
            contexts: (0..64)
                .map(|id| MessageContext {
                    id: format!("existing-{id}"),
                    label: "File".into(),
                    content: ContextContent::File {
                        path: "/host/file".into(),
                    },
                })
                .collect(),
            ..Default::default()
        });
        Arc::make_mut(&mut state.input.drafts).insert(key.clone(), full.clone());
        assert!(
            operation
                .complete(&mut state, "", "/host/capture.png".into(), true)
                .is_err()
        );
        assert!(
            Arc::ptr_eq(&state.input.drafts[&key], &full),
            "metadata failure cannot attach a partial capture"
        );
        let existing = Arc::new(Draft {
            contexts: vec![MessageContext {
                id: "capture".into(),
                label: "Existing".into(),
                content: ContextContent::File {
                    path: "/file".into(),
                },
            }],
            ..Default::default()
        });
        Arc::make_mut(&mut state.input.drafts).insert(key.clone(), existing.clone());
        assert!(
            upload()
                .complete(&mut state, "", "/host/capture.png".into(), true)
                .is_err()
        );
        assert!(Arc::ptr_eq(&state.input.drafts[&key], &existing));
    }
}
