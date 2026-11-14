//! Device-owned prompt content. Stash and composer updates share one persisted
//! snapshot, so a successful atomic save cannot leave only half of a move.
use crate::state::{Attachment, Draft, DraftKey, Input, Snapshot};
use agent_protocol::{
    composer::Invocation,
    context::{ContextContent, MessageContext},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

pub const STASH_LIMIT: usize = 20;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct StashedPrompt {
    pub id: String,
    pub created_at_ms: u64,
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub invocations: Vec<Invocation>,
    pub contexts: Vec<MessageContext>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum StashAction {
    Save { id: String, created_at_ms: u64 },
    Restore { id: String },
    Delete { id: String },
}

pub(crate) enum Change {
    Save { entry: StashedPrompt, draft: Draft },
    Restore { index: usize, draft: Draft },
    Delete { index: usize },
}

pub(crate) fn change(
    stashes: &[StashedPrompt],
    draft: &Draft,
    busy: bool,
    action: StashAction,
) -> Result<Option<Change>, &'static str> {
    match action {
        StashAction::Delete { id } => Ok(stashes
            .iter()
            .position(|entry| entry.id == id)
            .map(|index| Change::Delete { index })),
        StashAction::Restore { id } => {
            let Some(index) = stashes.iter().position(|entry| entry.id == id) else {
                return Ok(None);
            };
            if busy {
                return Err(
                    "finish attachment transfer, dictation or queue editing before restoring a prompt",
                );
            }
            Ok(Some(Change::Restore {
                index,
                draft: restore(draft, &stashes[index])?,
            }))
        }
        StashAction::Save { id, created_at_ms } => {
            if draft.text.trim().is_empty()
                && draft.attachments.is_empty()
                && draft.invocations.is_empty()
                && draft.contexts.is_empty()
            {
                return Ok(None);
            }
            if busy {
                return Err(
                    "finish attachment transfer, dictation or queue editing before stashing a prompt",
                );
            }
            if id.trim().is_empty()
                || id.encode_utf16().take(129).count() > 128
                || stashes.iter().any(|entry| entry.id == id)
            {
                return Err("invalid or duplicate stashed prompt identity");
            }
            if stashes.len() >= STASH_LIMIT {
                return Err("the prompt stash is full; remove an entry before stashing another");
            }
            agent_protocol::context::validate_contexts(&draft.contexts)?;
            let mut draft = draft.clone();
            let entry = StashedPrompt {
                id,
                created_at_ms,
                text: std::mem::take(&mut draft.text),
                attachments: std::mem::take(&mut draft.attachments),
                invocations: std::mem::take(&mut draft.invocations),
                contexts: std::mem::take(&mut draft.contexts),
            };
            Ok(Some(Change::Save { entry, draft }))
        }
    }
}

fn restore(current: &Draft, entry: &StashedPrompt) -> Result<Draft, &'static str> {
    agent_protocol::context::validate_contexts(&current.contexts)?;
    agent_protocol::context::validate_contexts(&entry.contexts)?;
    let mut draft = current.clone();
    if !entry.text.is_empty() {
        if draft.text.trim().is_empty() {
            draft.text.clear();
        } else {
            draft.text.truncate(draft.text.trim_end().len());
            draft.text.push_str("\n\n");
        }
        draft.text.push_str(&entry.text);
    }
    for attachment in &entry.attachments {
        if !draft
            .attachments
            .iter()
            .any(|existing| existing.path == attachment.path)
        {
            draft.attachments.push(attachment.clone());
        }
    }
    for invocation in &entry.invocations {
        if !draft.invocations.contains(invocation) {
            draft.invocations.push(invocation.clone());
        }
    }
    // Imported records get independent IDs. In particular a preview and its
    // screenshot must keep pointing to each other when an old ID is reused.
    let mut used: BTreeSet<_> = current
        .contexts
        .iter()
        .map(|context| context.id.clone())
        .collect();
    let mut ids = BTreeMap::new();
    let mut imported = entry.contexts.clone();
    for context in &mut imported {
        // n occupied IDs leave one of n+1 distinct candidates free.
        let id = (0..=used.len())
            .map(|index| format!("stash:{}:{index}", entry.id))
            .find(|id| !used.contains(id))
            .expect("a context identity is free");
        used.insert(id.clone());
        ids.insert(std::mem::replace(&mut context.id, id.clone()), id);
    }
    for context in &mut imported {
        if let ContextContent::Preview { preview } = &mut context.content
            && let Some(id) = preview
                .screenshot_context_id
                .as_ref()
                .and_then(|id| ids.get(id))
        {
            preview.screenshot_context_id = Some(id.clone());
        }
    }
    draft.contexts.extend(imported);
    agent_protocol::context::validate_contexts(&draft.contexts)?;
    Ok(draft)
}

impl Input {
    pub(crate) fn apply_stash_change(&mut self, key: DraftKey, change: Option<Change>) {
        match change {
            Some(Change::Save { entry, draft }) => {
                Arc::make_mut(&mut self.stashes).insert(0, entry);
                Arc::make_mut(&mut self.drafts).insert(key, Arc::new(draft));
            }
            Some(Change::Restore { index, draft }) => {
                Arc::make_mut(&mut self.stashes).remove(index);
                Arc::make_mut(&mut self.drafts).insert(key, Arc::new(draft));
            }
            Some(Change::Delete { index }) => {
                Arc::make_mut(&mut self.stashes).remove(index);
            }
            None => {}
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum StashedAttachmentState {
    Unverified,
    Available,
    Unavailable,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn stashed_attachment_state(&self, path: String) -> StashedAttachmentState {
        use agent_protocol::file_metadata::FileMetadataKind;
        match self.files.workspace.file_metadata.get(&path) {
            None => StashedAttachmentState::Unverified,
            Some(value)
                if value
                    .metadata
                    .as_ref()
                    .is_some_and(|metadata| metadata.kind == FileMetadataKind::File) =>
            {
                StashedAttachmentState::Available
            }
            Some(_) => StashedAttachmentState::Unavailable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        presentation::composer::ComposerPresentation,
        state::{Event, Intent, reduce},
    };
    use agent_protocol::{composer::InvocationKind, session::ProviderKind};

    fn file(id: &str, path: &str) -> MessageContext {
        MessageContext {
            id: id.into(),
            label: "File".into(),
            content: ContextContent::File { path: path.into() },
        }
    }
    fn content() -> Draft {
        Draft {
            text: "日本語 $review".into(),
            attachments: vec![Attachment {
                path: "/host/photo.png".into(),
                name: "photo.png".into(),
                is_image: true,
            }],
            invocations: vec![Invocation {
                provider: ProviderKind::Codex,
                kind: InvocationKind::Skill,
                name: "review".into(),
                path: "/skills/review".into(),
            }],
            contexts: vec![file("file", "/source.rs")],
            effort: Some("high".into()),
            service_tier: Some("fast".into()),
            model_settings_changed: true,
            ..Default::default()
        }
    }
    fn save() -> StashAction {
        StashAction::Save {
            id: "saved".into(),
            created_at_ms: 1234,
        }
    }

    #[test]
    fn save_preserves_every_content_part_and_settings_and_restore_appends_without_overwriting_choices()
     {
        let original = content();
        let Some(Change::Save {
            entry,
            draft: empty,
        }) = change(&[], &original, false, save()).unwrap()
        else {
            panic!("save");
        };
        assert_eq!(entry.text, original.text);
        assert_eq!(entry.attachments, original.attachments);
        assert_eq!(entry.invocations, original.invocations);
        assert_eq!(entry.contexts, original.contexts);
        assert_eq!(entry.created_at_ms, 1234);
        let mut expected = original.clone();
        expected.text.clear();
        expected.attachments.clear();
        expected.invocations.clear();
        expected.contexts.clear();
        assert_eq!(empty, expected);
        let current = Draft {
            text: "new question  \n".into(),
            effort: Some("low".into()),
            attachments: original.attachments.clone(),
            invocations: original.invocations.clone(),
            ..Default::default()
        };
        let restored = restore(&current, &entry).unwrap();
        assert_eq!(restored.text, "new question\n\n日本語 $review");
        assert_eq!(restored.attachments, original.attachments);
        assert_eq!(restored.invocations, original.invocations);
        assert_eq!(restored.effort, current.effort);
        assert_eq!(restored.model, current.model);
        assert_eq!(restored.workspace, current.workspace);
        assert_eq!(original, content());
    }

    #[test]
    fn a_prompt_with_any_single_content_kind_can_be_stashed() {
        let original = content();
        for draft in [
            Draft {
                text: original.text.clone(),
                ..Default::default()
            },
            Draft {
                attachments: original.attachments.clone(),
                ..Default::default()
            },
            Draft {
                invocations: original.invocations.clone(),
                ..Default::default()
            },
            Draft {
                contexts: original.contexts.clone(),
                ..Default::default()
            },
        ] {
            let Some(Change::Save {
                entry,
                draft: cleared,
            }) = change(&[], &draft, false, save()).unwrap()
            else {
                panic!("single content part must be saved");
            };
            assert_eq!(entry.text, draft.text);
            assert_eq!(entry.attachments, draft.attachments);
            assert_eq!(entry.invocations, draft.invocations);
            assert_eq!(entry.contexts, draft.contexts);
            assert_eq!(cleared, Draft::default());
        }
    }

    #[test]
    fn restoring_rekeys_context_and_internal_references_without_replacing_newer_context() {
        let mut original = content();
        original.contexts = vec![MessageContext { id: "shot".into(), label: "Image".into(), content: ContextContent::Image { path: "/old.png".into() } },MessageContext {
            id:"annotation".into(),label:"Preview".into(),content:ContextContent::Preview {
                preview:serde_json::from_value(serde_json::json!({"annotationId":"note","pageUrl":"https://example.com","pageTitle":null,"comment":"explain","targetSummary":"button","styleChanges":[],"elements":[],"elementIds":[],"regionCount":null,"strokeCount":null,"styleChangeDetails":[],"screenshotContextId":"shot"})).unwrap()
            }
        }];
        let Some(Change::Save { entry, .. }) = change(&[], &original, false, save()).unwrap()
        else {
            panic!("save");
        };
        let current = Draft {
            contexts: vec![
                file("shot", "/new.png"),
                file("stash:saved:0", "/other.png"),
            ],
            ..Default::default()
        };
        let restored = restore(&current, &entry).unwrap();
        assert_eq!(&restored.contexts[..2], current.contexts.as_slice());
        let imported = &restored.contexts[2..];
        assert_eq!(imported[0].content, original.contexts[0].content);
        assert_ne!(imported[0].id, "shot");
        let ContextContent::Preview { preview } = &imported[1].content else {
            panic!("preview");
        };
        assert_eq!(
            preview.screenshot_context_id.as_deref(),
            Some(imported[0].id.as_str())
        );
        assert_eq!(
            restored
                .contexts
                .iter()
                .map(|context| &context.id)
                .collect::<BTreeSet<_>>()
                .len(),
            4
        );
        assert_eq!(entry.contexts, original.contexts);
    }

    #[test]
    fn busy_empty_duplicate_and_full_stashes_never_consume_work_and_delete_remains_available() {
        let original = content();
        assert!(change(&[], &original, true, save()).is_err());
        assert!(
            change(
                &[],
                &Draft {
                    effort: Some("high".into()),
                    ..Default::default()
                },
                false,
                save()
            )
            .unwrap()
            .is_none()
        );
        let Some(Change::Save { entry, .. }) = change(&[], &original, false, save()).unwrap()
        else {
            panic!("save");
        };
        assert!(change(std::slice::from_ref(&entry), &original, false, save()).is_err());
        for id in [String::new(), " ".into(), "😀".repeat(65)] {
            assert!(
                change(
                    &[],
                    &original,
                    false,
                    StashAction::Save {
                        id,
                        created_at_ms: 1
                    }
                )
                .is_err()
            );
        }
        assert!(
            change(
                &[],
                &original,
                false,
                StashAction::Save {
                    id: "😀".repeat(64),
                    created_at_ms: 1
                }
            )
            .unwrap()
            .is_some()
        );
        assert!(
            change(
                &vec![entry.clone(); STASH_LIMIT],
                &original,
                false,
                StashAction::Save {
                    id: "next".into(),
                    created_at_ms: 2
                }
            )
            .is_err()
        );
        assert!(
            change(
                std::slice::from_ref(&entry),
                &original,
                true,
                StashAction::Restore {
                    id: entry.id.clone()
                }
            )
            .is_err()
        );
        assert!(matches!(
            change(
                std::slice::from_ref(&entry),
                &original,
                true,
                StashAction::Delete {
                    id: entry.id.clone()
                }
            )
            .unwrap(),
            Some(Change::Delete { index: 0 })
        ));
        assert!(
            change(
                &[entry],
                &original,
                false,
                StashAction::Restore { id: "gone".into() }
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(original, content());
    }

    #[test]
    fn context_capacity_failure_leaves_the_stashed_prompt_restorable() {
        let original = content();
        let Some(Change::Save { entry, .. }) = change(&[], &original, false, save()).unwrap()
        else {
            panic!("save");
        };
        let current = Draft {
            contexts: (0..64)
                .map(|id| file(&id.to_string(), "/source.rs"))
                .collect(),
            ..Default::default()
        };
        assert!(restore(&current, &entry).is_err());
        assert_eq!(entry.contexts, original.contexts);
        assert_eq!(current.contexts.len(), 64);
        assert_eq!(
            restore(&Draft::default(), &entry).unwrap().text,
            original.text
        );
    }

    #[test]
    fn stashed_attachments_distinguish_unchecked_files_and_unavailable_sources() {
        use agent_protocol::file_metadata::{FileMetadata, FileMetadataKind, FilePathMetadata};
        let mut state = Snapshot::default();
        assert_eq!(
            state.stashed_attachment_state("/new".into()),
            StashedAttachmentState::Unverified
        );
        let cache = &mut Arc::make_mut(&mut state.files.workspace).file_metadata;
        let (generation, paths) =
            cache.ensure(vec!["/ready".into(), "/gone".into(), "/folder".into()], 0);
        let values = paths
            .iter()
            .map(|path| FilePathMetadata {
                path: path.clone(),
                metadata: match path.as_str() {
                    "/ready" => Some(FileMetadata {
                        kind: FileMetadataKind::File,
                        byte_length: Some(12),
                        mime_type: Some("image/png".into()),
                    }),
                    "/folder" => Some(FileMetadata {
                        kind: FileMetadataKind::Directory,
                        byte_length: None,
                        mime_type: None,
                    }),
                    _ => None,
                },
            })
            .collect();
        cache.received(generation, paths, Some(values), 1);
        assert_eq!(
            state.stashed_attachment_state("/ready".into()),
            StashedAttachmentState::Available
        );
        assert_eq!(
            state.stashed_attachment_state("/gone".into()),
            StashedAttachmentState::Unavailable
        );
        assert_eq!(
            state.stashed_attachment_state("/folder".into()),
            StashedAttachmentState::Unavailable
        );
        assert_eq!(
            state.stashed_attachment_state("/new".into()),
            StashedAttachmentState::Unverified
        );
    }

    #[test]
    fn stashes_persist_with_the_cleared_composer_and_stay_in_their_host_storage_scope() {
        let (mut state, _) = reduce(
            &Snapshot::default(),
            "",
            Event::Intent(Intent::NewChat {
                cwd: "/project".into(),
            }),
        );
        state.connection.storage_scope = "a".into();
        let key = state.panes.primary().navigation.draft_key.clone();
        Arc::make_mut(&mut state.input.drafts).insert(key.clone(), Arc::new(content()));
        let intent = |scope: &str, action| {
            Event::Intent(Intent::Stash {
                storage_scope: scope.into(),
                thread_id: key.clone(),
                action,
            })
        };
        let (unchanged, _) = reduce(&state, "", intent("other", save()));
        assert!(unchanged.input.stashes.is_empty());
        assert_eq!(unchanged.input.drafts[&key].as_ref(), &content());
        let (saved, effects) = reduce(&state, "", intent("a", save()));
        assert!(effects.is_empty());
        assert_eq!(saved.input.stashes.len(), 1);
        assert!(saved.input.drafts[&key].text.is_empty());
        let presentation = ComposerPresentation::project(&saved, saved.panes.primary(), None);
        assert_eq!(presentation.stash_count(), 1);
        assert_eq!(
            presentation.stashed_prompts(),
            saved.input.stashes.as_ref().clone()
        );
        let restored =
            crate::persistence::decode(&crate::persistence::encode(&saved).unwrap()).unwrap();
        assert_eq!(restored.input.stashes, saved.input.stashes);
        assert_eq!(restored.input.drafts, saved.input.drafts);
        let (other, _) = reduce(&restored, "", Event::StorageScope("b".into()));
        assert!(other.input.stashes.is_empty());
        let (back, _) = reduce(&other, "", Event::StorageScope("a".into()));
        assert_eq!(back.input.stashes, saved.input.stashes);
        let (resumed, _) = reduce(
            &back,
            "",
            intent("a", StashAction::Restore { id: "saved".into() }),
        );
        assert!(resumed.input.stashes.is_empty());
        assert_eq!(resumed.input.drafts[&key].text, content().text);
        assert_eq!(
            resumed.input.drafts[&key].attachments,
            content().attachments
        );
        assert!(state.input.stashes.is_empty());
    }

    #[tokio::test]
    async fn offline_store_dispatch_moves_prompts_and_updates_the_shared_composer() {
        let (mut initial, _) = reduce(
            &Snapshot::default(),
            "",
            Event::Intent(Intent::NewChat {
                cwd: "/project".into(),
            }),
        );
        initial.connection.storage_scope = "host".into();
        let key = initial.panes.primary().navigation.draft_key.clone();
        Arc::make_mut(&mut initial.input.drafts).insert(key.clone(), Arc::new(content()));
        let store = crate::store::Store::offline(initial, None);
        let intent = |action| Intent::Stash {
            storage_scope: "host".into(),
            thread_id: key.clone(),
            action,
        };
        store.dispatch(intent(save())).await.unwrap();
        assert_eq!(store.presentation().composer.stash_count(), 1);
        assert!(store.snapshot().input.drafts[&key].text.is_empty());
        for id in ["two", "three"] {
            store
                .dispatch(Intent::SetDraftText {
                    thread_id: key.clone(),
                    text: id.into(),
                })
                .await
                .unwrap();
            store
                .dispatch(intent(StashAction::Save {
                    id: id.into(),
                    created_at_ms: 2000,
                }))
                .await
                .unwrap();
        }
        assert_eq!(
            store
                .presentation()
                .composer
                .stashed_prompts()
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            ["three", "two", "saved"]
        );
        store
            .dispatch(Intent::SetDraftText {
                thread_id: key.clone(),
                text: "current".into(),
            })
            .await
            .unwrap();
        store
            .dispatch(intent(StashAction::Delete { id: "two".into() }))
            .await
            .unwrap();
        assert_eq!(store.snapshot().input.drafts[&key].text, "current");
        store
            .dispatch(intent(StashAction::Restore { id: "saved".into() }))
            .await
            .unwrap();
        assert_eq!(store.presentation().composer.stash_count(), 1);
        assert_eq!(
            store.presentation().composer.stashed_prompts()[0].id,
            "three"
        );
        assert_eq!(
            store.snapshot().input.drafts[&key].text,
            format!("current\n\n{}", content().text)
        );
        assert_eq!(
            store.snapshot().input.drafts[&key].attachments,
            content().attachments
        );
        store.close().await.unwrap();
    }

    proptest::proptest! {
        #[test]
        fn context_import_never_overwrites_a_draft_record(occupied in 0usize..32, incoming in 0usize..33) {
            let current = Draft {contexts:(0..occupied).map(|index|file(&format!("stash:saved:{index}"),&format!("/current/{index}"))).collect(),..Default::default()};
            let entry = StashedPrompt {id:"saved".into(),created_at_ms:1,text:"saved".into(),attachments:vec![],invocations:vec![],contexts:(0..incoming).map(|index|file(&format!("saved-{index}"),&format!("/saved/{index}"))).collect()};
            let restored = restore(&current,&entry).unwrap();
            proptest::prop_assert_eq!(&restored.contexts[..occupied],current.contexts.as_slice());
            let ids:BTreeSet<_> = restored.contexts.iter().map(|context|&context.id).collect();
            proptest::prop_assert_eq!(ids.len(),occupied+incoming);
            let contents:Vec<_> = restored.contexts[occupied..].iter().map(|context|&context.content).collect();
            proptest::prop_assert_eq!(contents,entry.contexts.iter().map(|context|&context.content).collect::<Vec<_>>());
        }
    }
}
