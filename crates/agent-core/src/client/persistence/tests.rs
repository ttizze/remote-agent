use super::*;
use crate::persistence::EditorPreferences;
use crate::state::{Draft, Intent, ModelSelection};
use agent_protocol::{editors::EditorId, models::ModelRef, session::ProviderKind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Default)]
struct DeviceDisk {
    models: Mutex<Vec<u8>>,
    editor: Mutex<Vec<u8>>,
    composer: Mutex<Vec<u8>>,
    writes: AtomicUsize,
    fail_models: AtomicBool,
}
#[derive(Default)]
struct Storage {
    local: Mutex<Vec<u8>>,
    local_writes: AtomicUsize,
    device: Arc<DeviceDisk>,
    fail_local: AtomicBool,
    saved: tokio::sync::Notify,
}
impl ClientStorage for Storage {
    fn load(&self) -> Result<Vec<u8>, AgentError> {
        Ok(self.local.lock().unwrap().clone())
    }
    fn save(&self, bytes: Vec<u8>) -> Result<(), AgentError> {
        if self.fail_local.load(Ordering::SeqCst) {
            return Err(error("disk unavailable"));
        }
        *self.local.lock().unwrap() = bytes;
        self.local_writes.fetch_add(1, Ordering::SeqCst);
        self.device.writes.fetch_add(1, Ordering::SeqCst);
        self.saved.notify_one();
        Ok(())
    }
}
impl PreferenceStorage for DeviceDisk {
    fn load(&self, key: PreferenceKey) -> Result<Vec<u8>, AgentError> {
        Ok(match key {
            PreferenceKey::Models => self.models.lock().unwrap().clone(),
            PreferenceKey::Editor => self.editor.lock().unwrap().clone(),
            PreferenceKey::Composer => self.composer.lock().unwrap().clone(),
            PreferenceKey::Language | PreferenceKey::Contrast => Vec::new(),
        })
    }
    fn save(&self, key: PreferenceKey, bytes: Vec<u8>) -> Result<(), AgentError> {
        match key {
            PreferenceKey::Models => {
                if self.fail_models.load(Ordering::SeqCst) {
                    return Err(error("preferences unavailable"));
                }
                *self.models.lock().unwrap() = bytes;
            }
            PreferenceKey::Editor => *self.editor.lock().unwrap() = bytes,
            PreferenceKey::Composer => *self.composer.lock().unwrap() = bytes,
            PreferenceKey::Language | PreferenceKey::Contrast => {
                panic!("this fixture never edits language")
            }
        }
        self.writes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
fn persistence(storage: Arc<Storage>) -> Arc<Persistence> {
    let device = DevicePreferences::new(storage.device.clone());
    Persistence::new(storage, device)
}
fn accept(
    device: &DevicePreferences,
    snapshot: &Snapshot,
    choice: ModelChoice,
    provider: ProviderKind,
) {
    device
        .apply_model(
            choice,
            snapshot.catalog.model_preferences.selections[&provider].clone(),
        )
        .unwrap();
    device.publish();
}
fn draft(snapshot: &mut Snapshot, text: &str) {
    Arc::make_mut(&mut snapshot.input.drafts).insert(
        "draft".into(),
        Arc::new(Draft {
            text: text.into(),
            ..Default::default()
        }),
    );
}
fn selection(
    provider: ProviderKind,
    id: &str,
    effort: Option<String>,
    tier: Option<String>,
) -> ModelSelection {
    ModelSelection {
        model: ModelRef {
            provider,
            id: id.into(),
        },
        effort,
        service_tier: tier,
    }
}
fn remember(snapshot: &mut Snapshot, selection: ModelSelection) {
    let provider = selection.model.provider;
    snapshot.catalog.model_preferences.provider = Some(provider);
    snapshot
        .catalog
        .model_preferences
        .selections
        .insert(provider, selection);
}
fn selected(snapshot: &Snapshot) -> &ModelSelection {
    &snapshot.catalog.model_preferences.selections[&ProviderKind::Codex]
}
fn with_disk(disk: Arc<DeviceDisk>) -> Arc<Storage> {
    Arc::new(Storage {
        device: disk,
        ..Default::default()
    })
}

#[tokio::test]
async fn device_preferences_apply_across_hosts_without_replacing_their_work() {
    let mut defaults = Snapshot::default();
    remember(
        &mut defaults,
        selection(
            ProviderKind::Codex,
            "chosen",
            Some("high".into()),
            Some("priority".into()),
        ),
    );
    defaults.files.preferred_editor = Some(EditorId::Zed);
    let disk = Arc::new(DeviceDisk::default());
    *disk.models.lock().unwrap() = serde_json::to_vec(&defaults.catalog.model_preferences).unwrap();
    *disk.editor.lock().unwrap() = serde_json::to_vec(&EditorPreferences {
        preferred: defaults.files.preferred_editor,
    })
    .unwrap();
    let device = DevicePreferences::new(disk.clone());
    for host in ["first", "second"] {
        let storage = with_disk(disk.clone());
        let mut saved = Snapshot {
            connection: crate::state::Connection {
                storage_scope: host.into(),
                ..Default::default()
            },
            ..Default::default()
        };
        draft(&mut saved, &format!("{host}'s draft"));
        *storage.local.lock().unwrap() = crate::persistence::encode(&saved).unwrap();
        let restored = Persistence::new(storage, device.clone())
            .restore(false, None)
            .await
            .unwrap();
        assert_eq!(restored.input.drafts, saved.input.drafts);
        assert_eq!(
            restored.panes.primary().navigation,
            saved.panes.primary().navigation
        );
        assert_eq!(restored.connection.storage_scope, host);
        assert_eq!(
            restored.catalog.model_preferences,
            defaults.catalog.model_preferences
        );
        assert_eq!(restored.files.preferred_editor, Some(EditorId::Zed));
    }
    assert_eq!(disk.writes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn simultaneous_views_keep_each_providers_choice_when_closed_in_reverse_order() {
    let disk = Arc::new(DeviceDisk::default());
    let device = DevicePreferences::new(disk.clone());
    let main_storage = with_disk(disk.clone());
    let side_storage = with_disk(disk.clone());
    let main = Persistence::new(main_storage.clone(), device.clone());
    let side = Persistence::new(side_storage.clone(), device.clone());
    let mut first = main.restore(false, None).await.unwrap();
    let mut second = side.restore(false, None).await.unwrap();
    tokio::spawn(
        main.start(watch::channel(Arc::new(first.clone())).1)
            .unwrap(),
    );
    tokio::spawn(
        side.start(watch::channel(Arc::new(second.clone())).1)
            .unwrap(),
    );
    let codex = selection(ProviderKind::Codex, "preferred", Some("high".into()), None);
    let claude = selection(ProviderKind::Claude, "other", None, Some("fast".into()));
    remember(&mut first, codex.clone());
    remember(&mut second, claude.clone());
    accept(&device, &first, ModelChoice::Model, ProviderKind::Codex);
    draft(&mut first, "main draft");
    draft(&mut second, "side draft");
    main.enqueue(Arc::new(first.clone()))
        .unwrap()
        .await
        .unwrap();
    accept(&device, &second, ModelChoice::Model, ProviderKind::Claude);
    side.enqueue(Arc::new(second.clone()))
        .unwrap()
        .await
        .unwrap();
    side.finish(Arc::new(second)).unwrap().await.unwrap();
    main.finish(Arc::new(first)).unwrap().await.unwrap();
    let restored = persistence(with_disk(disk))
        .restore(false, None)
        .await
        .unwrap();
    assert_eq!(
        restored.catalog.model_preferences.provider,
        Some(ProviderKind::Claude)
    );
    assert_eq!(
        restored.catalog.model_preferences.selections[&ProviderKind::Codex],
        codex
    );
    assert_eq!(
        restored.catalog.model_preferences.selections[&ProviderKind::Claude],
        claude
    );
    let main = crate::persistence::decode(&main_storage.local.lock().unwrap()).unwrap();
    let side = crate::persistence::decode(&side_storage.local.lock().unwrap()).unwrap();
    assert_eq!(main.input.drafts[&"draft".into()].text, "main draft");
    assert_eq!(side.input.drafts[&"draft".into()].text, "side draft");
    assert!(restored.input.drafts.is_empty());
}

#[tokio::test]
async fn simultaneous_option_edits_do_not_overwrite_each_other() {
    let disk = Arc::new(DeviceDisk::default());
    let mut initial = Snapshot::default();
    remember(
        &mut initial,
        selection(ProviderKind::Codex, "chosen", None, None),
    );
    *disk.models.lock().unwrap() = serde_json::to_vec(&initial.catalog.model_preferences).unwrap();
    let device = DevicePreferences::new(disk.clone());
    let main = Persistence::new(with_disk(disk.clone()), device.clone());
    let side = Persistence::new(with_disk(disk.clone()), device.clone());
    let mut first = main.restore(false, None).await.unwrap();
    let mut second = side.restore(false, None).await.unwrap();
    tokio::spawn(
        main.start(watch::channel(Arc::new(first.clone())).1)
            .unwrap(),
    );
    tokio::spawn(
        side.start(watch::channel(Arc::new(second.clone())).1)
            .unwrap(),
    );
    first
        .catalog
        .model_preferences
        .selections
        .get_mut(&ProviderKind::Codex)
        .unwrap()
        .effort = Some("high".into());
    second
        .catalog
        .model_preferences
        .selections
        .get_mut(&ProviderKind::Codex)
        .unwrap()
        .service_tier = Some("priority".into());
    accept(&device, &first, ModelChoice::Effort, ProviderKind::Codex);
    accept(
        &device,
        &second,
        ModelChoice::ServiceTier,
        ProviderKind::Codex,
    );
    main.enqueue(Arc::new(first.clone()))
        .unwrap()
        .await
        .unwrap();
    side.finish(Arc::new(second)).unwrap().await.unwrap();
    main.finish(Arc::new(first)).unwrap().await.unwrap();
    let restored = persistence(with_disk(disk))
        .restore(false, None)
        .await
        .unwrap();
    assert_eq!(selected(&restored).effort.as_deref(), Some("high"));
    assert_eq!(
        selected(&restored).service_tier.as_deref(),
        Some("priority")
    );
}

#[tokio::test]
async fn retrying_a_failed_local_save_does_not_reapply_obsolete_device_edits() {
    let disk = Arc::new(DeviceDisk::default());
    let device = DevicePreferences::new(disk.clone());
    let storage = with_disk(disk.clone());
    let first = Persistence::new(storage.clone(), device.clone());
    let second = Persistence::new(with_disk(disk), device.clone());
    let mut high = first.restore(false, None).await.unwrap();
    let mut low = second.restore(false, None).await.unwrap();
    tokio::spawn(
        first
            .start(watch::channel(Arc::new(high.clone())).1)
            .unwrap(),
    );
    tokio::spawn(
        second
            .start(watch::channel(Arc::new(low.clone())).1)
            .unwrap(),
    );
    remember(
        &mut high,
        selection(ProviderKind::Codex, "chosen", Some("high".into()), None),
    );
    accept(&device, &high, ModelChoice::Model, ProviderKind::Codex);
    draft(&mut high, "recover this draft");
    storage.fail_local.store(true, Ordering::SeqCst);
    assert!(
        first
            .enqueue(Arc::new(high.clone()))
            .unwrap()
            .await
            .is_err()
    );
    assert!(first.failure().unwrap().contains("disk unavailable"));
    remember(
        &mut low,
        selection(ProviderKind::Codex, "chosen", Some("low".into()), None),
    );
    accept(&device, &low, ModelChoice::Effort, ProviderKind::Codex);
    second.finish(Arc::new(low)).unwrap().await.unwrap();
    storage.fail_local.store(false, Ordering::SeqCst);
    first.finish(Arc::new(high)).unwrap().await.unwrap();
    assert!(first.failure().is_none());
    let restored = persistence(storage).restore(false, None).await.unwrap();
    assert_eq!(selected(&restored).effort.as_deref(), Some("low"));
    assert_eq!(
        restored.input.drafts[&"draft".into()].text,
        "recover this draft"
    );
}

#[tokio::test]
async fn an_option_edit_reselects_its_provider_without_erasing_the_other_windows_choice() {
    let disk = Arc::new(DeviceDisk::default());
    let mut initial = Snapshot::default();
    remember(
        &mut initial,
        selection(ProviderKind::Codex, "chosen", None, None),
    );
    *disk.models.lock().unwrap() = serde_json::to_vec(&initial.catalog.model_preferences).unwrap();
    let device = DevicePreferences::new(disk.clone());
    let main = Persistence::new(with_disk(disk.clone()), device.clone());
    let side = Persistence::new(with_disk(disk.clone()), device.clone());
    let mut first = main.restore(false, None).await.unwrap();
    let mut second = side.restore(false, None).await.unwrap();
    tokio::spawn(
        main.start(watch::channel(Arc::new(first.clone())).1)
            .unwrap(),
    );
    tokio::spawn(
        side.start(watch::channel(Arc::new(second.clone())).1)
            .unwrap(),
    );
    remember(
        &mut second,
        selection(ProviderKind::Claude, "other", None, None),
    );
    accept(&device, &second, ModelChoice::Model, ProviderKind::Claude);
    side.enqueue(Arc::new(second.clone()))
        .unwrap()
        .await
        .unwrap();
    first
        .catalog
        .model_preferences
        .selections
        .get_mut(&ProviderKind::Codex)
        .unwrap()
        .effort = Some("high".into());
    accept(&device, &first, ModelChoice::Effort, ProviderKind::Codex);
    main.finish(Arc::new(first)).unwrap().await.unwrap();
    side.finish(Arc::new(second)).unwrap().await.unwrap();
    let restored = persistence(with_disk(disk))
        .restore(false, None)
        .await
        .unwrap();
    assert_eq!(
        restored.catalog.model_preferences.provider,
        Some(ProviderKind::Codex)
    );
    assert_eq!(selected(&restored).effort.as_deref(), Some("high"));
    assert_eq!(
        restored.catalog.model_preferences.selections[&ProviderKind::Claude]
            .model
            .id,
        "other"
    );
}

#[tokio::test]
async fn corrupted_state_or_preferences_are_rejected_without_writing() {
    for key in [
        None,
        Some(PreferenceKey::Models),
        Some(PreferenceKey::Editor),
        Some(PreferenceKey::Composer),
    ] {
        let storage = Arc::new(Storage::default());
        match key {
            None => *storage.local.lock().unwrap() = b"{}".to_vec(),
            Some(PreferenceKey::Models) => {
                *storage.device.models.lock().unwrap() = b"invalid".to_vec()
            }
            Some(PreferenceKey::Editor) => {
                *storage.device.editor.lock().unwrap() = b"invalid".to_vec()
            }
            Some(PreferenceKey::Composer) => {
                *storage.device.composer.lock().unwrap() = b"invalid".to_vec()
            }
            Some(PreferenceKey::Language | PreferenceKey::Contrast) => unreachable!(),
        }
        assert!(
            persistence(storage.clone())
                .restore(false, None)
                .await
                .is_err()
        );
        assert_eq!(storage.device.writes.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn failed_device_writes_remain_dirty_when_another_session_saves() {
    let disk = Arc::new(DeviceDisk::default());
    let device = DevicePreferences::new(disk.clone());
    let first = Persistence::new(with_disk(disk.clone()), device.clone());
    let second = Persistence::new(with_disk(disk.clone()), device.clone());
    let mut high = first.restore(false, None).await.unwrap();
    let mut other = second.restore(false, None).await.unwrap();
    tokio::spawn(
        first
            .start(watch::channel(Arc::new(high.clone())).1)
            .unwrap(),
    );
    tokio::spawn(
        second
            .start(watch::channel(Arc::new(other.clone())).1)
            .unwrap(),
    );
    remember(
        &mut high,
        selection(ProviderKind::Codex, "chosen", Some("high".into()), None),
    );
    disk.fail_models.store(true, Ordering::SeqCst);
    accept(&device, &high, ModelChoice::Model, ProviderKind::Codex);
    assert!(
        first
            .enqueue(Arc::new(high.clone()))
            .unwrap()
            .await
            .is_err()
    );
    assert!(disk.models.lock().unwrap().is_empty());
    disk.fail_models.store(false, Ordering::SeqCst);
    other.files.preferred_editor = Some(EditorId::Zed);
    device.apply_editor(EditorId::Zed).unwrap();
    device.publish();
    second.finish(Arc::new(other)).unwrap().await.unwrap();
    first.finish(Arc::new(high)).unwrap().await.unwrap();
    let restored = persistence(with_disk(disk))
        .restore(false, None)
        .await
        .unwrap();
    assert_eq!(selected(&restored).effort.as_deref(), Some("high"));
    assert_eq!(restored.files.preferred_editor, Some(EditorId::Zed));
}

#[tokio::test]
async fn final_save_seals_the_queue_even_if_earlier_waiters_are_cancelled() {
    let storage = Arc::new(Storage::default());
    let persistence = persistence(storage.clone());
    let mut snapshot = persistence.restore(false, None).await.unwrap();
    let snapshots = watch::channel(Arc::new(snapshot.clone())).1;
    let task = persistence.start(snapshots.clone()).unwrap();
    assert!(persistence.start(snapshots).is_none());
    let saving = tokio::spawn(task);
    draft(&mut snapshot, "earlier");
    drop(persistence.enqueue(Arc::new(snapshot.clone())).unwrap());
    draft(&mut snapshot, "final");
    let final_save = persistence.finish(Arc::new(snapshot.clone())).unwrap();
    assert!(persistence.enqueue(Arc::new(Snapshot::default())).is_err());
    final_save.await.unwrap();
    saving.await.unwrap();
    assert_eq!(
        crate::persistence::decode(&storage.local.lock().unwrap())
            .unwrap()
            .input
            .drafts,
        snapshot.input.drafts
    );
}

#[rstest::rstest]
#[case::leaving(false)]
#[case::quitting(true)]
#[tokio::test]
async fn a_change_in_the_copies_quiet_period_is_written_before_the_app_stops(
    #[case] quitting: bool,
) {
    use crate::models::Thread;
    use agent_protocol::session::SessionRef;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let directory = tempfile::tempdir().unwrap();
        let copies = directory.path().join("copies");
        let storage = Arc::new(Storage::default());
        let persistence = persistence(storage.clone());
        let mut snapshot = persistence
            .restore(false, Some(copies.clone()))
            .await
            .unwrap();
        let id = SessionRef::new(ProviderKind::Codex, "cached".into()).unwrap();
        let named = |name: &str| {
            Arc::new(Thread {
                id: Some(id.clone()),
                name: Some(name.into()),
                turns: Some(Vec::new()),
                ..Default::default()
            })
        };
        Arc::make_mut(&mut snapshot.sessions.conversations).insert(id.clone(), named("first"));
        let (updates, snapshots) = watch::channel(Arc::new(snapshot));
        let saving = tokio::spawn(persistence.start(snapshots).unwrap());
        let saved_name = || async {
            self::persistence(storage.clone())
                .restore(false, Some(copies.clone()))
                .await
                .unwrap()
                .sessions
                .conversations
                .get(&id)
                .and_then(|thread| thread.name.clone())
        };
        while saved_name().await.as_deref() != Some("first") {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        // The copies now wait ten quiet seconds before their next write.
        let mut renamed = updates.borrow().as_ref().clone();
        Arc::make_mut(&mut renamed.sessions.conversations).insert(id.clone(), named("second"));
        let renamed = Arc::new(renamed);
        updates.send_replace(renamed.clone());
        if quitting {
            // As a session quits: the copies first, then the final state.
            persistence.flush_copies().await;
            persistence.finish(renamed).unwrap().await.unwrap();
        } else {
            persistence.flush(renamed).unwrap().await.unwrap();
        }
        assert_eq!(saved_name().await.as_deref(), Some("second"));
        saving.abort();
    })
    .await
    .expect("conversation copies stalled");
}

#[tokio::test]
async fn conversation_copies_are_saved_restored_and_cleared_on_pairing() {
    use crate::models::{Thread, ThreadList};
    use agent_protocol::session::SessionRef;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let directory = tempfile::tempdir().unwrap();
        let copies = directory.path().join("copies");
        let storage = Arc::new(Storage::default());
        let persistence = persistence(storage.clone());
        let mut snapshot = persistence
            .restore(false, Some(copies.clone()))
            .await
            .unwrap();
        let id = SessionRef::new(ProviderKind::Codex, "cached".into()).unwrap();
        let thread = Thread {
            id: Some(id.clone()),
            name: Some("saved conversation".into()),
            turns: Some(Vec::new()),
            ..Default::default()
        };
        snapshot.connection.connected = true;
        snapshot.lists.threads = Some(Arc::new(ThreadList {
            revision: 1,
            limit: 5,
            data: vec![thread.clone()],
            projects: Vec::new(),
            has_more: false,
            has_more_projects: false,
            project_pages: Default::default(),
            provider_errors: None,
        }));
        Arc::make_mut(&mut snapshot.sessions.conversations).insert(id.clone(), Arc::new(thread));
        draft(&mut snapshot, "host draft");
        remember(
            &mut snapshot,
            selection(ProviderKind::Codex, "chosen", None, None),
        );
        accept(
            &persistence.device,
            &snapshot,
            ModelChoice::Model,
            ProviderKind::Codex,
        );
        let (updates, snapshots) = watch::channel(Arc::new(snapshot));
        let saving = tokio::spawn(persistence.start(snapshots).unwrap());
        let snapshot = updates.borrow().clone();
        persistence.finish(snapshot).unwrap().await.unwrap();
        let restored = loop {
            let restored = self::persistence(storage.clone())
                .restore(false, Some(copies.clone()))
                .await
                .unwrap();
            if restored.sessions.conversations.contains_key(&id) && restored.thread_list().is_some()
            {
                break restored;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        assert_eq!(
            restored.sessions.conversations[&id].name.as_deref(),
            Some("saved conversation")
        );
        assert_eq!(
            restored.thread_list().unwrap().threads[0].title,
            "saved conversation"
        );
        assert_eq!(restored.input.drafts[&"draft".into()].text, "host draft");
        assert_eq!(selected(&restored).model.id, "chosen");
        saving.abort();
        assert!(saving.await.unwrap_err().is_cancelled());
        // A new pairing must ignore the old Host state, even if it is damaged.
        *storage.local.lock().unwrap() = b"damaged previous Host".to_vec();
        let paired = self::persistence(storage)
            .restore(true, Some(copies.clone()))
            .await
            .unwrap();
        assert!(paired.sessions.conversations.is_empty());
        assert!(paired.thread_list().is_none());
        assert!(paired.input.drafts.is_empty());
        assert_eq!(selected(&paired).model.id, "chosen");
        assert!(!copies.exists());
    })
    .await
    .expect("conversation persistence stalled");
}

#[tokio::test]
async fn autosave_observes_core_changes_without_ui_notifications() {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        let storage = Arc::new(Storage::default());
        let persistence = persistence(storage.clone());
        let restored = persistence.restore(false, None).await.unwrap();
        let store = crate::store::Store::offline(restored, Some(persistence.device.clone()));
        tokio::spawn(persistence.start(store.subscribe()).unwrap());
        let stop = CancellationToken::new();
        let observe = tokio::spawn(persistence.clone().observe(store.subscribe(), stop.clone()));
        store
            .dispatch(Intent::SetDraftText {
                thread_id: "draft".into(),
                text: "no UI save".into(),
            })
            .await
            .unwrap();
        loop {
            storage.saved.notified().await;
            let snapshot = crate::persistence::decode(&storage.local.lock().unwrap()).unwrap();
            if snapshot
                .input
                .drafts
                .get(&"draft".into())
                .is_some_and(|draft| draft.text == "no UI save")
            {
                break;
            }
        }
        stop.cancel();
        observe.await.unwrap();
        store.close().await.unwrap();
        persistence.finish(store.snapshot()).unwrap().await.unwrap();
    })
    .await
    .expect("autosave stalled");
}

#[test]
fn model_replacement_preserves_other_providers_and_resets_its_options() {
    let device = DevicePreferences::memory(Preferences::default());
    let claude = selection(
        ProviderKind::Claude,
        "other",
        Some("medium".into()),
        Some("fast".into()),
    );
    device
        .apply_model(ModelChoice::Model, claude.clone())
        .unwrap();
    device
        .apply_model(
            ModelChoice::Model,
            selection(
                ProviderKind::Codex,
                "old",
                Some("high".into()),
                Some("priority".into()),
            ),
        )
        .unwrap();
    let codex = selection(ProviderKind::Codex, "new", None, None);
    device
        .apply_model(ModelChoice::Model, codex.clone())
        .unwrap();
    let current = device.current();
    assert_eq!(current.models.selections[&ProviderKind::Codex], codex);
    assert_eq!(current.models.selections[&ProviderKind::Claude], claude);
    assert_eq!(current.models.provider, Some(ProviderKind::Codex));
}

#[test]
fn stale_model_options_never_attach_to_a_different_models_selection() {
    let device = DevicePreferences::memory(Preferences::default());
    device
        .apply_model(
            ModelChoice::Model,
            selection(ProviderKind::Codex, "new", None, Some("priority".into())),
        )
        .unwrap();
    let old = selection(ProviderKind::Codex, "old", Some("high".into()), None);
    device
        .apply_model(ModelChoice::Effort, old.clone())
        .unwrap();
    assert_eq!(
        device.current().models.selections[&ProviderKind::Codex],
        old
    );
}

proptest::proptest! {
    #[test]
    fn independent_option_edits_commute(
        original_effort in proptest::option::of("[a-z]{1,8}"),
        original_tier in proptest::option::of("[a-z]{1,8}"),
        effort in proptest::option::of("[a-z]{1,8}"),
        tier in proptest::option::of("[a-z]{1,8}"),
    ) {
        let baseline = selection(ProviderKind::Codex, "chosen", original_effort.clone(), original_tier.clone());
        let first = selection(ProviderKind::Codex, "chosen", effort.clone(), original_tier);
        let second = selection(ProviderKind::Codex, "chosen", original_effort, tier.clone());
        let apply = |edits: [(ModelChoice, ModelSelection); 2]| {
            let device = DevicePreferences::memory(Preferences::default());
            device.apply_model(ModelChoice::Model, baseline.clone()).unwrap();
            for (choice, selection) in edits {
                device.apply_model(choice, selection).unwrap();
            }
            device.current().models.clone()
        };
        let forward = apply([(ModelChoice::Effort, first.clone()), (ModelChoice::ServiceTier, second.clone())]);
        let backward = apply([(ModelChoice::ServiceTier, second), (ModelChoice::Effort, first)]);
        proptest::prop_assert_eq!(&forward, &backward);
        proptest::prop_assert_eq!(&forward.selections[&ProviderKind::Codex].effort, &effort);
        proptest::prop_assert_eq!(&forward.selections[&ProviderKind::Codex].service_tier, &tier);
    }
}

#[tokio::test]
async fn autosave_ignores_display_changes_without_postponing_pending_drafts() {
    use crate::localization::UiLanguage;
    let storage = Arc::new(Storage::default());
    let persistence = persistence(storage.clone());
    let mut snapshot = persistence.restore(false, None).await.unwrap();
    let (updates, snapshots) = watch::channel(Arc::new(snapshot.clone()));
    let saving = tokio::spawn(persistence.start(snapshots.clone()).unwrap());
    let stop = CancellationToken::new();
    let observe = tokio::spawn(persistence.clone().observe(snapshots, stop.clone()));
    storage.saved.notified().await;
    assert_eq!(storage.local_writes.load(Ordering::SeqCst), 1);

    snapshot.catalog.ui_language = UiLanguage::English;
    updates.send_replace(Arc::new(snapshot.clone()));
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    // The explicit save is a barrier for any earlier automatic writes.
    persistence
        .enqueue(Arc::new(snapshot.clone()))
        .unwrap()
        .await
        .unwrap();
    assert_eq!(storage.local_writes.load(Ordering::SeqCst), 2);

    draft(&mut snapshot, "save while display updates continue");
    updates.send_replace(Arc::new(snapshot.clone()));
    for index in 0..10 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        snapshot.catalog.ui_language = if index % 2 == 0 {
            UiLanguage::Japanese
        } else {
            UiLanguage::English
        };
        updates.send_replace(Arc::new(snapshot.clone()));
    }
    persistence
        .enqueue(Arc::new(snapshot.clone()))
        .unwrap()
        .await
        .unwrap();
    assert_eq!(storage.local_writes.load(Ordering::SeqCst), 4);
    assert_eq!(
        crate::persistence::decode(&storage.local.lock().unwrap())
            .unwrap()
            .input
            .drafts[&"draft".into()]
            .text,
        "save while display updates continue"
    );
    stop.cancel();
    observe.await.unwrap();
    persistence
        .finish(Arc::new(snapshot))
        .unwrap()
        .await
        .unwrap();
    saving.await.unwrap();
    assert_eq!(storage.local_writes.load(Ordering::SeqCst), 5);
}

#[tokio::test]
async fn autosave_keeps_each_kind_of_host_owned_work() {
    let storage = Arc::new(Storage::default());
    let persistence = persistence(storage.clone());
    let mut snapshot = persistence.restore(false, None).await.unwrap();
    let (updates, snapshots) = watch::channel(Arc::new(snapshot.clone()));
    let saving = tokio::spawn(persistence.start(snapshots.clone()).unwrap());
    let stop = CancellationToken::new();
    let observe = tokio::spawn(persistence.clone().observe(snapshots, stop.clone()));
    storage.saved.notified().await;
    let edits: &[fn(&mut Snapshot)] = &[
        |snapshot| snapshot.connection.connection_disabled = true,
        |snapshot| {
            Arc::make_mut(&mut snapshot.lists.project_preferences)
                .insert("host".into(), Default::default());
        },
        |snapshot| snapshot.connection.storage_scope = "paired-host".into(),
        |snapshot| {
            Arc::make_mut(&mut snapshot.lists.archived_scopes)
                .insert("old-host".into(), Arc::default());
        },
        |snapshot| draft(snapshot, "unsent work"),
        |snapshot| {
            Arc::make_mut(&mut snapshot.sessions.pending_submissions).insert(
                "pending".into(),
                Arc::new(crate::state::PendingSubmission {
                    sequence: 1,
                    submitted_at_ms: None,
                    draft_key: "draft".into(),
                    draft: Arc::new(Draft {
                        text: "queued work".into(),
                        ..Default::default()
                    }),
                    turn_id: None,
                    after_item_id: None,
                    accepted: false,
                    delivery_unknown: false,
                    queued: false,
                }),
            );
        },
        |snapshot| {
            Arc::make_mut(&mut snapshot.files.file_drafts).insert(
                "/file.txt".into(),
                Arc::new(crate::state::FileDraft {
                    revision: "base".into(),
                    text: "unsaved file".into(),
                }),
            );
        },
        |snapshot| {
            let pane = Arc::make_mut(snapshot.panes.primary_mut());
            Arc::make_mut(&mut pane.navigation).cwd = "/workspace".into();
        },
    ];
    for edit in edits {
        let before = storage.local.lock().unwrap().clone();
        let count = storage.local_writes.load(Ordering::SeqCst);
        edit(&mut snapshot);
        updates.send_replace(Arc::new(snapshot.clone()));
        tokio::time::timeout(std::time::Duration::from_secs(3), storage.saved.notified())
            .await
            .expect("host work was not automatically saved");
        assert_eq!(storage.local_writes.load(Ordering::SeqCst), count + 1);
        assert_ne!(*storage.local.lock().unwrap(), before);
    }
    stop.cancel();
    observe.await.unwrap();
    persistence
        .finish(Arc::new(snapshot))
        .unwrap()
        .await
        .unwrap();
    saving.await.unwrap();
}

#[tokio::test]
async fn device_failure_and_recovery_do_not_require_a_local_state_write() {
    let disk = Arc::new(DeviceDisk::default());
    let device = DevicePreferences::new(disk.clone());
    let storage = with_disk(disk.clone());
    let persistence = Persistence::new(storage.clone(), device.clone());
    let mut snapshot = persistence.restore(false, None).await.unwrap();
    let saving = tokio::spawn(
        persistence
            .start(watch::channel(Arc::new(snapshot.clone())).1)
            .unwrap(),
    );
    disk.fail_models.store(true, Ordering::SeqCst);
    remember(
        &mut snapshot,
        selection(ProviderKind::Codex, "chosen", Some("high".into()), None),
    );
    accept(&device, &snapshot, ModelChoice::Model, ProviderKind::Codex);
    assert!(
        persistence
            .enqueue(Arc::new(snapshot.clone()))
            .unwrap()
            .await
            .is_err()
    );
    assert!(
        persistence
            .failure()
            .unwrap()
            .contains("preferences unavailable")
    );
    assert_eq!(storage.local_writes.load(Ordering::SeqCst), 0);
    disk.fail_models.store(false, Ordering::SeqCst);
    accept(&device, &snapshot, ModelChoice::Model, ProviderKind::Codex);
    device.flush().await.unwrap();
    assert!(persistence.failure().is_none());
    assert_eq!(storage.local_writes.load(Ordering::SeqCst), 0);
    persistence
        .finish(Arc::new(snapshot))
        .unwrap()
        .await
        .unwrap();
    saving.await.unwrap();
    assert_eq!(storage.local_writes.load(Ordering::SeqCst), 1);
}
