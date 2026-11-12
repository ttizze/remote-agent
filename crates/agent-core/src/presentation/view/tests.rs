use super::*;
use crate::{
    session::{ProviderKind, SessionRef},
    state::{
        DraftKey, Intent, Navigation,
        operations::ReadThread,
        projects::{PlacedChat, ProjectPreferences},
    },
    store::Store,
};

fn session(id: &str) -> SessionRef {
    SessionRef::new(ProviderKind::Codex, id.into()).unwrap()
}

fn snapshot() -> Snapshot {
    let mut state = Snapshot::default();
    Arc::make_mut(state.panes.primary_mut()).navigation = Arc::new(Navigation {
        thread_id: Some(session("a")),
        draft_key: session("a").into(),
        cwd: "/a".into(),
    });
    state.sessions.conversations = Arc::new(
        ["a", "b"]
            .map(|id| {
                let id = session(id);
                (
                    id.clone(),
                    Arc::new(crate::models::Thread {
                        id: Some(id),
                        name: Some("Title".into()),
                        ..Default::default()
                    }),
                )
            })
            .into(),
    );
    state.lists.threads = Some(Arc::new(
        serde_json::from_value(serde_json::json!({
            "data":[],"projects":[{"id":"project","name":"Project","roots":[]}],
            "hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5
        }))
        .unwrap(),
    ));
    state.lists.expanded_projects = Arc::new([("project".into(), 5)].into());
    state.lists.project_preferences = Arc::new(
        [(
            "".into(),
            [(
                "project".into(),
                ProjectPreferences {
                    placed_chats: vec![PlacedChat {
                        id: session("a"),
                        title: "Saved title".into(),
                    }],
                    ..Default::default()
                },
            )]
            .into(),
        )]
        .into(),
    );
    state
}

#[test]
fn workspace_review_publications_refresh_the_native_composer_summary() {
    use agent_protocol::models::{ChangedFile, WorkspaceReview};
    let mut state = snapshot();
    let mut previous = Presentation::project(Arc::new(state.clone()), "", None).unwrap();
    for (additions, deletions) in [(7, 2), (19, 5)] {
        Arc::make_mut(&mut Arc::make_mut(state.panes.primary_mut()).workspace).review =
            Some(Arc::new(WorkspaceReview {
                controls: Default::default(),
                options: None,
                base_ref: Some("origin/main".into()),
                branch: "main".into(),
                additions,
                deletions,
                files: vec![ChangedFile {
                    path: "src/main.rs".into(),
                    status: "M".into(),
                    additions: Some(additions),
                    deletions: Some(deletions),
                }],
                diff: String::new(),
            }));
        let next = Presentation::project(Arc::new(state.clone()), "", Some(&previous)).unwrap();
        assert!(next.changes(previous).groups.panes);
        let review = next.composer_review(false).unwrap();
        assert_eq!(
            (review.files, review.additions, review.deletions),
            (1, additions, deletions)
        );
        previous = next;
    }
    Arc::make_mut(&mut Arc::make_mut(state.panes.primary_mut()).workspace).review = None;
    let next = Presentation::project(Arc::new(state), "", Some(&previous)).unwrap();
    assert!(next.changes(previous).groups.panes);
    assert_eq!(next.composer_review(false), None);
}

#[tokio::test]
async fn navigation_and_cancellation_do_not_wait_for_transcript_projection() {
    use std::sync::{Mutex, mpsc};
    use std::time::Duration;

    for cancel in [false, true] {
        let (release, gate) = mpsc::channel();
        let gate = Mutex::new(Some(gate));
        let (started, waiting) = tokio::sync::oneshot::channel();
        let started = Mutex::new(Some(started));
        let (writer, snapshots) = watch::channel(Arc::new(snapshot()));
        let stop = CancellationToken::new();
        let mut views = observe(
            snapshots.clone(),
            Presentation::pending(snapshots.borrow().clone(), "").unwrap(),
            stop.clone(),
            move |snapshot, pane_id, previous| {
                if let Some(gate) = gate.lock().unwrap().take() {
                    started.lock().unwrap().take().unwrap().send(()).unwrap();
                    // Dropping release on test failure also releases this worker.
                    let _ = gate.recv();
                }
                Presentation::project(snapshot, pane_id, previous)
            },
        );
        tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap();
        let mut next = snapshot();
        Arc::make_mut(next.panes.primary_mut()).navigation = Arc::new(Navigation {
            thread_id: Some(session("b")),
            draft_key: session("b").into(),
            cwd: "/b".into(),
        });
        writer.send_replace(Arc::new(next));
        let acknowledged = tokio::time::timeout(
            Duration::from_secs(2),
            views.wait_for(|view| view.snapshot.panes.primary().navigation.cwd == "/b"),
        )
        .await
        .expect("navigation must not wait for transcript projection")
        .unwrap()
        .clone();
        assert_eq!(
            acknowledged.snapshot.panes.primary().navigation.thread_id,
            Some(session("b"))
        );
        assert!(
            acknowledged.conversation.is_none(),
            "old navigation's transcript must be hidden"
        );
        if cancel {
            stop.cancel();
            assert!(
                tokio::time::timeout(Duration::from_secs(2), views.changed())
                    .await
                    .unwrap()
                    .is_err(),
                "closing a pane must not wait for its transcript worker"
            );
            release.send(()).unwrap();
            continue;
        }
        release.send(()).unwrap();
        let rendered = tokio::time::timeout(
            Duration::from_secs(2),
            views.wait_for(|view| {
                view.conversation
                    .as_ref()
                    .is_some_and(|conversation| conversation.source.id == Some(session("b")))
            }),
        )
        .await
        .expect("the worker must catch up to the acknowledged navigation")
        .unwrap()
        .clone();
        assert_eq!(rendered.snapshot.panes.primary().navigation.cwd, "/b");
        assert!(
            tokio::time::timeout(Duration::from_millis(30), views.changed())
                .await
                .is_err(),
            "an idle projection must not publish repeated frames"
        );
        stop.cancel();
    }
}

#[test]
fn conversation_chrome_uses_current_titles_without_waiting_for_transcript_projection() {
    let initial = Presentation::project(Arc::new(snapshot()), "", None).unwrap();
    assert_eq!(initial.context.title().as_deref(), Some("Title"));
    let mut next = (*initial.snapshot).clone();
    Arc::make_mut(
        Arc::make_mut(&mut next.sessions.conversations)
            .get_mut(&session("a"))
            .unwrap(),
    )
    .name = None;
    Arc::make_mut(next.lists.threads.as_mut().unwrap())
        .data
        .push(crate::models::Thread {
            id: Some(session("a")),
            name: Some("Listed title".into()),
            ..Default::default()
        });
    let current = Presentation::current(Arc::new(next), &initial).unwrap();
    assert_eq!(current.context.title().as_deref(), Some("Listed title"));
    assert!(current.changes(initial.clone()).context);
    assert!(Arc::ptr_eq(
        current.conversation.as_ref().unwrap(),
        initial.conversation.as_ref().unwrap()
    ));
}

#[test]
fn body_deltas_reuse_the_list_but_shortcut_metadata_updates_it() {
    let initial = Presentation::project(Arc::new(snapshot()), "", None).unwrap();
    let mut state = initial.snapshot.as_ref().clone();
    Arc::make_mut(
        Arc::make_mut(&mut state.sessions.conversations)
            .get_mut(&session("a"))
            .unwrap(),
    )
    .turns = Some(Vec::new());
    let streamed = Presentation::project(Arc::new(state), "", Some(&initial)).unwrap();
    let changes = streamed.changes(initial.clone());
    assert!(changes.conversation);
    assert!(!changes.composer_controls);
    assert_eq!(
        streamed.composer.draft.attachments,
        initial.composer.draft.attachments
    );
    assert!(Arc::ptr_eq(
        &streamed.conversation.as_ref().unwrap().source_paths,
        &initial.conversation.as_ref().unwrap().source_paths,
    ));
    assert!(Arc::ptr_eq(&streamed.composer, &initial.composer));
    assert!(Arc::ptr_eq(&streamed.context, &initial.context));
    assert!(Arc::ptr_eq(
        streamed.list.as_ref().unwrap(),
        initial.list.as_ref().unwrap()
    ));
    assert!(!Arc::ptr_eq(
        streamed.conversation.as_ref().unwrap(),
        initial.conversation.as_ref().unwrap()
    ));
    let mut state = streamed.snapshot.as_ref().clone();
    Arc::make_mut(
        Arc::make_mut(&mut state.sessions.conversations)
            .get_mut(&session("a"))
            .unwrap(),
    )
    .name = Some("Renamed".into());
    let renamed = Presentation::project(Arc::new(state), "", Some(&streamed)).unwrap();
    assert!(!Arc::ptr_eq(
        renamed.list.as_ref().unwrap(),
        streamed.list.as_ref().unwrap()
    ));
    assert_eq!(
        renamed.list.as_ref().unwrap().projects[0].threads[0].title,
        "Renamed"
    );
    assert_eq!(
        initial.list.as_ref().unwrap().projects[0].threads[0].title,
        "Title"
    );
}

#[test]
fn draft_attachments_model_controls_and_transcript_sources_have_distinct_subjects() {
    use agent_protocol::items::{ItemBody, MessagePart};
    let initial = Presentation::project(Arc::new(snapshot()), "", None).unwrap();
    let mut state = (*initial.snapshot).clone();
    let key = state.panes.primary().navigation.draft_key.clone();
    Arc::make_mut(&mut state.input.drafts).insert(
        key.clone(),
        Arc::new(crate::state::Draft {
            attachments: vec![crate::state::Attachment {
                path: "/a/picture.png".into(),
                name: "picture.png".into(),
                is_image: true,
            }],
            ..Default::default()
        }),
    );
    let attached = Presentation::project(Arc::new(state.clone()), "", Some(&initial)).unwrap();
    let changes = attached.changes(initial.clone());
    assert_ne!(
        attached.composer.draft.attachments,
        initial.composer.draft.attachments
    );
    assert!(changes.composer);
    assert!(!changes.composer_controls);
    assert!(Arc::ptr_eq(
        &attached.conversation.as_ref().unwrap().source_paths,
        &initial.conversation.as_ref().unwrap().source_paths,
    ));
    Arc::make_mut(
        Arc::make_mut(&mut state.input.drafts)
            .get_mut(&key)
            .unwrap(),
    )
    .model = Some(crate::models::ModelRef {
        provider: ProviderKind::Codex,
        id: "chosen".into(),
    });
    let chosen = Presentation::project(Arc::new(state.clone()), "", Some(&attached)).unwrap();
    let changes = chosen.changes(attached.clone());
    assert!(changes.composer_controls);
    assert_eq!(
        chosen.composer.draft.attachments,
        attached.composer.draft.attachments
    );
    assert!(Arc::ptr_eq(
        &chosen.conversation.as_ref().unwrap().source_paths,
        &attached.conversation.as_ref().unwrap().source_paths,
    ));
    let file = Arc::new(crate::models::Item::new(
        "file".into(),
        Default::default(),
        ItemBody::UserMessage {
            text: Some("source".into()),
            content: vec![MessagePart::Attachment {
                name: "source.rs".into(),
                path: "/a/source.rs".into(),
            }],
        },
    ));
    Arc::make_mut(
        Arc::make_mut(&mut state.sessions.conversations)
            .get_mut(&session("a"))
            .unwrap(),
    )
    .turns = Some(vec![Arc::new(crate::models::Turn {
        items: Some(vec![file]),
        ..Default::default()
    })]);
    let sourced = Presentation::project(Arc::new(state), "", Some(&chosen)).unwrap();
    assert!(!Arc::ptr_eq(
        &sourced.conversation.as_ref().unwrap().source_paths,
        &chosen.conversation.as_ref().unwrap().source_paths,
    ));
    assert_eq!(
        sourced
            .conversation
            .as_ref()
            .unwrap()
            .source_paths
            .as_slice(),
        ["/a/source.rs"]
    );
}

#[test]
fn only_the_selected_draft_changes_its_composer() {
    let initial = Presentation::project(Arc::new(snapshot()), "", None).unwrap();
    let mut state = initial.snapshot.as_ref().clone();
    crate::state::set_draft_text(&mut state, session("b").into(), "other pane".into());
    let other = Presentation::project(Arc::new(state.clone()), "", Some(&initial)).unwrap();
    assert!(Arc::ptr_eq(&other.composer, &initial.composer));
    assert!(Arc::ptr_eq(&other.context, &initial.context));
    crate::state::set_draft_text(&mut state, session("a").into(), "this pane".into());
    let selected = Presentation::project(Arc::new(state), "", Some(&other)).unwrap();
    assert!(!Arc::ptr_eq(&selected.composer, &other.composer));
    assert!(Arc::ptr_eq(&selected.context, &other.context));
    assert!(Arc::ptr_eq(
        selected.conversation.as_ref().unwrap(),
        other.conversation.as_ref().unwrap()
    ));
    assert_eq!(selected.composer.draft.text, "this pane");
}

#[test]
fn rebasing_retains_a_stream_prefix_and_drops_another_selection_or_host() {
    let initial = Presentation::project(Arc::new(snapshot()), "", None).unwrap();
    let mut state = initial.snapshot.as_ref().clone();
    crate::state::set_draft_text(&mut state, session("a").into(), "typed".into());
    let current = Presentation::current(Arc::new(state.clone()), &initial).unwrap();
    assert_eq!(
        current.snapshot.input.drafts[&DraftKey::from(session("a"))].text,
        "typed"
    );
    assert!(Arc::ptr_eq(
        current.conversation.as_ref().unwrap(),
        initial.conversation.as_ref().unwrap()
    ));
    Arc::make_mut(&mut Arc::make_mut(state.panes.primary_mut()).navigation).thread_id =
        Some(session("b"));
    assert!(
        Presentation::current(Arc::new(state.clone()), &initial)
            .unwrap()
            .conversation
            .is_none()
    );
    assert!(!initial.same_context(&state));
    state = initial.snapshot.as_ref().clone();
    state.connection.storage_scope = "another host".into();
    let replaced = Presentation::current(Arc::new(state), &initial).unwrap();
    assert!(replaced.list.is_none() && replaced.conversation.is_none());
}

async fn presented(store: &Store, id: &SessionRef) -> Arc<Presentation> {
    let mut updates = store.subscribe_presentation();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let value = updates.borrow_and_update().clone();
            if value
                .conversation
                .as_ref()
                .is_some_and(|conversation| conversation.source.id.as_ref() == Some(id))
            {
                return value;
            }
            updates.changed().await.unwrap();
        }
    })
    .await
    .expect("presentation did not follow navigation")
}

#[tokio::test]
async fn the_native_stream_keeps_acknowledgments_and_selected_history_consistent() {
    let store = Store::offline(snapshot(), None);
    let initial = presented(&store, &session("a")).await;
    store
        .dispatch(Intent::SetDraftText {
            thread_id: session("a").into(),
            text: "latest draft".into(),
        })
        .await
        .unwrap();
    let acknowledged = store.presentation();
    assert_eq!(
        acknowledged.snapshot.input.drafts[&DraftKey::from(session("a"))].text,
        "latest draft"
    );
    assert!(Arc::ptr_eq(
        acknowledged.conversation.as_ref().unwrap(),
        initial.conversation.as_ref().unwrap()
    ));
    assert!(
        store
            .dispatch(Intent::ReadThread(ReadThread::open(session("b"))))
            .await
            .is_err()
    );
    let immediate = store.presentation();
    assert_eq!(
        immediate.snapshot.panes.primary().navigation.thread_id,
        Some(session("b"))
    );
    assert!(
        immediate
            .conversation
            .as_ref()
            .is_none_or(|conversation| conversation.source.id == Some(session("b")))
    );
    let selected = presented(&store, &session("b")).await;
    assert_eq!(
        selected.snapshot.panes.primary().navigation.thread_id,
        Some(session("b"))
    );
    assert_eq!(
        selected.snapshot.input.drafts[&DraftKey::from(session("a"))].text,
        "latest draft"
    );
    store.close().await.unwrap();
}

#[test]
fn language_changes_replace_localized_content_and_reject_old_projection_context() {
    use crate::{localization::UiLanguage, presentation::conversation::ConversationRowContent};
    let mut state = snapshot();
    let thread = Arc::make_mut(&mut state.sessions.conversations)
        .get_mut(&session("a"))
        .unwrap();
    Arc::make_mut(thread).turns = Some(vec![Arc::new(crate::models::Turn {
        id: "work".into(),
        status: crate::models::TurnStatus::Completed,
        items: Some(vec![Arc::new(crate::models::Item::new(
            "answer".into(),
            crate::models::ItemStatus::Completed,
            crate::models::ItemBody::AssistantText {
                text: "answer".into(),
                phase: crate::models::AssistantPhase::Final,
            },
        ))]),
        items_summary: true,
        duration_ms: Some(1000),
        ..Default::default()
    })]);
    let japanese = Presentation::project(Arc::new(state.clone()), "", None).unwrap();
    state.catalog.ui_language = UiLanguage::English;
    assert!(
        !japanese.same_context(&state),
        "a background result in the previous language must not be published"
    );
    let english = Presentation::project(Arc::new(state), "", Some(&japanese)).unwrap();
    let conversation = english.conversation.as_ref().unwrap();
    assert_eq!(
        conversation.turns[0]
            .rows
            .iter()
            .find_map(|row| match &row.content {
                ConversationRowContent::ActivityHeader { activity } =>
                    Some(activity.activity_summary.as_str()),
                _ => None,
            }),
        Some("Worked for 1.0s")
    );
    let unchanged = Presentation::project(english.snapshot.clone(), "", Some(&english)).unwrap();
    assert!(Arc::ptr_eq(
        conversation,
        unchanged.conversation.as_ref().unwrap()
    ));
}

#[test]
fn language_changes_refresh_lazy_settings_and_composer_copy_without_host_data() {
    use crate::localization::UiLanguage;
    let japanese = Presentation::empty(UiLanguage::Japanese);
    let mut next = japanese.snapshot.as_ref().clone();
    next.catalog.ui_language = UiLanguage::English;
    let english = Presentation::project(Arc::new(next), "", Some(&japanese)).unwrap();
    let changes = english.changes(japanese.clone());
    assert!(changes.settings && changes.composer);
    assert_eq!(english.settings.connection_status_label(), "Disconnected");
    assert_eq!(english.settings.permission.label, "No agent selected");
    assert_eq!(
        english
            .composer
            .composer_suggestions("/".into(), 1)
            .unwrap()
            .status
            .as_deref(),
        Some("Loading suggestions…")
    );
    assert_eq!(japanese.settings.connection_status_label(), "未接続");
    assert_eq!(
        japanese
            .composer
            .composer_suggestions("/".into(), 1)
            .unwrap()
            .status
            .as_deref(),
        Some("候補を読み込み中…")
    );
    let unchanged = Presentation::project(english.snapshot.clone(), "", Some(&english)).unwrap();
    assert!(Arc::ptr_eq(&english.settings, &unchanged.settings));
    assert!(Arc::ptr_eq(&english.composer, &unchanged.composer));
}

#[test]
fn another_panes_navigation_reuses_the_held_conversation_and_controls() {
    let mut state = snapshot();
    state.open_pane("side".into(), "/b".into());
    Arc::make_mut(state.panes.get_mut("side").unwrap()).navigation = Arc::new(Navigation {
        thread_id: Some(session("b")),
        draft_key: session("b").into(),
        cwd: "/b".into(),
    });
    let first = Presentation::project(Arc::new(state.clone()), "side", None).unwrap();
    let (next, _) = crate::state::reduce(
        &state,
        "",
        crate::state::Event::Intent(Intent::NewChat {
            cwd: "/other".into(),
        }),
    );
    let second = Presentation::project(Arc::new(next), "side", Some(&first)).unwrap();
    assert!(Arc::ptr_eq(first.pane(), second.pane()));
    assert!(Arc::ptr_eq(
        first.conversation.as_ref().unwrap(),
        second.conversation.as_ref().unwrap()
    ));
    assert!(Arc::ptr_eq(&first.composer, &second.composer));
    assert!(Arc::ptr_eq(&first.context, &second.context));
    assert!(Arc::ptr_eq(&first.agents, &second.agents));
    assert!(Arc::ptr_eq(&first.settings, &second.settings));
    assert!(!second.changes(first).groups.panes);
}
