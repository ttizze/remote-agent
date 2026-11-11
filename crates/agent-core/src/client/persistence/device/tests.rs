use super::*;
use crate::{
    state::{Draft, Intent, Snapshot},
    store::Store,
};
use agent_protocol::{models::ModelRef, session::ProviderKind};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct Disk {
    bytes: Mutex<[Vec<u8>; 5]>,
    fail: AtomicUsize,
    writes: Mutex<Vec<usize>>,
}
fn index(key: PreferenceKey) -> usize {
    match key {
        PreferenceKey::Models => 0,
        PreferenceKey::Editor => 1,
        PreferenceKey::Language => 2,
        PreferenceKey::Composer => 3,
        PreferenceKey::Contrast => 4,
    }
}
impl PreferenceStorage for Disk {
    fn load(&self, key: PreferenceKey) -> Result<Vec<u8>, AgentError> {
        Ok(self.bytes.lock().unwrap()[index(key)].clone())
    }
    fn save(&self, key: PreferenceKey, bytes: Vec<u8>) -> Result<(), AgentError> {
        let index = index(key);
        if self.fail.load(Ordering::SeqCst) == index + 1 {
            return Err(error("disk unavailable"));
        }
        self.bytes.lock().unwrap()[index] = bytes;
        self.writes.lock().unwrap().push(index);
        Ok(())
    }
}

#[tokio::test]
async fn boot_language_precedes_hosts_and_later_restoration_keeps_live_settings() {
    let disk = Arc::new(Disk::default());
    disk.bytes.lock().unwrap()[2] = br#""en""#.to_vec();
    let device = DevicePreferences::new(disk.clone());
    assert_eq!(device.language(), UiLanguage::English);
    assert!(disk.writes.lock().unwrap().is_empty());
    let first = Store::offline(Snapshot::default(), Some(device.clone()));
    assert_eq!(first.snapshot().catalog.ui_language, UiLanguage::English);
    device.select_language(UiLanguage::Japanese).unwrap();
    device.apply_editor(EditorId::Zed).unwrap();
    device.publish();
    // A later Host has stale/default settings. They never become device edits.
    let late = Store::offline(Snapshot::default(), Some(device.clone()));
    assert_eq!(late.snapshot().files.preferred_editor, Some(EditorId::Zed));
    assert_eq!(late.snapshot().catalog.ui_language, UiLanguage::Japanese);
    let mut updates = first.subscribe();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        updates.wait_for(|value| {
            value.catalog.ui_language == UiLanguage::Japanese
                && value.files.preferred_editor == Some(EditorId::Zed)
        }),
    )
    .await
    .unwrap()
    .unwrap();
    device.flush().await.unwrap();
    assert_eq!(
        DevicePreferences::new(disk).language(),
        UiLanguage::Japanese
    );
    first.close().await.unwrap();
    late.close().await.unwrap();
}

#[tokio::test]
async fn language_write_failure_keeps_the_visible_value_and_allows_reselection() {
    let disk = Arc::new(Disk::default());
    let device = DevicePreferences::new(disk.clone());
    let mut updates = device.subscribe();
    let before = device.current();
    disk.fail.store(3, Ordering::SeqCst);
    assert!(device.select_language(UiLanguage::English).is_err());
    assert_eq!(device.language(), UiLanguage::Japanese);
    assert!(Arc::ptr_eq(&before, &device.current()));
    assert!(!updates.has_changed().unwrap());
    assert!(disk.bytes.lock().unwrap()[2].is_empty());
    disk.fail.store(0, Ordering::SeqCst);
    device.select_language(UiLanguage::English).unwrap();
    assert_eq!(device.language(), UiLanguage::English);
    assert_eq!(updates.borrow_and_update().language, UiLanguage::English);
    assert_eq!(
        device.next_language(UiLanguage::Japanese).await.unwrap(),
        UiLanguage::English
    );
    device.select_language(UiLanguage::English).unwrap();
    assert!(!updates.has_changed().unwrap());
    assert_eq!(*disk.writes.lock().unwrap(), vec![2, 2]);
    assert_eq!(DevicePreferences::new(disk).language(), UiLanguage::English);
}

#[tokio::test]
async fn damaged_language_does_not_block_hosts_or_get_rewritten_by_unrelated_saves() {
    let disk = Arc::new(Disk::default());
    disk.bytes.lock().unwrap()[2] = b"damaged".to_vec();
    let device = DevicePreferences::new(disk.clone());
    assert_eq!(device.language(), UiLanguage::Japanese);
    assert!(device.restore().is_ok());
    device.apply_editor(EditorId::Cursor).unwrap();
    device.publish();
    device.flush().await.unwrap();
    assert_eq!(disk.bytes.lock().unwrap()[2], b"damaged");
    assert!(!disk.writes.lock().unwrap().contains(&2));
    device.select_language(UiLanguage::Japanese).unwrap();
    assert_eq!(disk.bytes.lock().unwrap()[2], br#""ja""#);
}

#[tokio::test]
async fn damaged_model_or_editor_blocks_restore_until_repaired_without_losing_language_edits() {
    for broken in 0..2 {
        let disk = Arc::new(Disk::default());
        disk.bytes.lock().unwrap()[broken] = b"damaged".to_vec();
        let device = DevicePreferences::new(disk.clone());
        assert!(device.restore().is_err());
        assert!(device.apply_editor(EditorId::Zed).is_err());
        assert!(device.flush().await.is_err());
        assert_eq!(disk.bytes.lock().unwrap()[broken], b"damaged");
        assert!(disk.writes.lock().unwrap().is_empty());
        device.select_language(UiLanguage::English).unwrap();
        disk.bytes.lock().unwrap()[broken].clear();
        assert_eq!(device.restore().unwrap().language, UiLanguage::English);
        device.apply_editor(EditorId::Zed).unwrap();
        device.publish();
        device.flush().await.unwrap();
        assert_eq!(device.current().editor.preferred, Some(EditorId::Zed));
        assert!(device.failure().is_none());
    }
}

#[tokio::test]
async fn editor_is_applied_in_memory_and_retried_without_rewriting_saved_models() {
    let disk = Arc::new(Disk::default());
    let device = DevicePreferences::new(disk.clone());
    let mut failures = device.failures();
    device.flush().await.unwrap();
    assert!(!failures.has_changed().unwrap());
    disk.writes.lock().unwrap().clear();
    disk.fail.store(2, Ordering::SeqCst);
    device.apply_editor(EditorId::Zed).unwrap();
    device.publish();
    assert_eq!(device.current().editor.preferred, Some(EditorId::Zed));
    assert!(device.flush().await.is_err());
    assert!(device.failure().unwrap().contains("disk unavailable"));
    assert!(disk.writes.lock().unwrap().is_empty());
    assert!(failures.borrow_and_update().is_some());
    // Reselecting after dismissing a failure must report another failed attempt.
    device.apply_editor(EditorId::Zed).unwrap();
    device.publish();
    assert!(device.flush().await.is_err());
    assert!(failures.has_changed().unwrap());
    failures.borrow_and_update();
    disk.fail.store(0, Ordering::SeqCst);
    // Reselecting the current value is a valid save retry.
    device.apply_editor(EditorId::Zed).unwrap();
    device.publish();
    device.flush().await.unwrap();
    assert!(device.failure().is_none());
    assert!(failures.has_changed().unwrap());
    assert!(failures.borrow_and_update().is_none());
    device.flush().await.unwrap();
    assert!(!failures.has_changed().unwrap());
    assert_eq!(*disk.writes.lock().unwrap(), vec![1]);
    assert_eq!(
        DevicePreferences::new(disk).current().editor.preferred,
        Some(EditorId::Zed)
    );
}

#[tokio::test]
async fn stores_share_accepted_preferences_without_overwriting_host_work_or_draft_options() {
    let device = DevicePreferences::memory(Preferences::default());
    let initial = |host: &str| {
        let mut value = Snapshot::default();
        value.connection.storage_scope = host.into();
        Arc::make_mut(&mut value.input.drafts).insert(
            "draft".into(),
            Arc::new(Draft {
                text: host.into(),
                model: Some(ModelRef {
                    provider: ProviderKind::Codex,
                    id: "chosen".into(),
                }),
                effort: Some("low".into()),
                ..Default::default()
            }),
        );
        value
    };
    let first = Store::offline(initial("first"), Some(device.clone()));
    let second = Store::offline(initial("second"), Some(device.clone()));
    let first_before = first.snapshot();
    first
        .dispatch(Intent::SelectEffort {
            thread_id: "draft".into(),
            effort: "high".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        device.current().models.selections[&ProviderKind::Codex]
            .effort
            .as_deref(),
        Some("high")
    );
    second
        .dispatch(Intent::SelectServiceTier {
            thread_id: "draft".into(),
            service_tier: "priority".into(),
        })
        .await
        .unwrap();
    second
        .dispatch(Intent::SetPreferredEditor {
            editor: EditorId::Zed,
        })
        .await
        .unwrap();
    let settled = |value: &Arc<Snapshot>| {
        value
            .catalog
            .model_preferences
            .selections
            .get(&ProviderKind::Codex)
            .is_some_and(|selection| {
                selection.effort.as_deref() == Some("high")
                    && selection.service_tier.as_deref() == Some("priority")
            })
            && value.files.preferred_editor == Some(EditorId::Zed)
    };
    for store in [&first, &second] {
        let mut updates = store.subscribe();
        tokio::time::timeout(std::time::Duration::from_secs(2), updates.wait_for(settled))
            .await
            .unwrap()
            .unwrap();
    }
    assert_eq!(first.snapshot().input.drafts[&"draft".into()].text, "first");
    assert_eq!(
        second.snapshot().input.drafts[&"draft".into()].text,
        "second"
    );
    assert_eq!(first.snapshot().connection.storage_scope, "first");
    assert_eq!(second.snapshot().connection.storage_scope, "second");
    assert_eq!(
        second.snapshot().input.drafts[&"draft".into()]
            .effort
            .as_deref(),
        Some("low")
    );
    assert_eq!(
        first_before.input.drafts[&"draft".into()].effort.as_deref(),
        Some("low")
    );
    first.close().await.unwrap();
    second.close().await.unwrap();
    let before = device.current();
    assert!(
        first
            .dispatch(Intent::SetPreferredEditor {
                editor: EditorId::Cursor
            })
            .await
            .is_err()
    );
    assert!(Arc::ptr_eq(&before, &device.current()));
}

#[tokio::test]
async fn preference_notifications_allow_synchronous_store_reads() {
    use futures_util::task::{ArcWake, waker};
    use std::{
        future::Future,
        task::{Context, Poll},
    };
    struct Reader {
        store: Arc<Store>,
        calls: AtomicUsize,
    }
    impl ArcWake for Reader {
        fn wake_by_ref(reader: &Arc<Self>) {
            assert_eq!(
                reader.store.snapshot().files.preferred_editor,
                Some(EditorId::Zed)
            );
            reader.calls.fetch_add(1, Ordering::SeqCst);
        }
    }
    let device = DevicePreferences::memory(Preferences::default());
    let store = Arc::new(Store::offline(Snapshot::default(), Some(device.clone())));
    let mut updates = device.subscribe();
    let changed = updates.changed();
    futures_util::pin_mut!(changed);
    let reader = Arc::new(Reader {
        store: store.clone(),
        calls: AtomicUsize::new(0),
    });
    let wake = waker(reader.clone());
    assert!(matches!(
        changed.as_mut().poll(&mut Context::from_waker(&wake)),
        Poll::Pending
    ));
    store
        .dispatch(Intent::SetPreferredEditor {
            editor: EditorId::Zed,
        })
        .await
        .unwrap();
    assert_eq!(reader.calls.load(Ordering::SeqCst), 1);
    let before = store.snapshot();
    store
        .dispatch(Intent::SetPreferredEditor {
            editor: EditorId::Zed,
        })
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&before, &store.snapshot()));
    store.close().await.unwrap();
}

#[test]
fn contrast_defaults_saves_and_stays_in_range() {
    use crate::presentation::appearance::{DEFAULT_CONTRAST, MAX_CONTRAST};
    let disk = Arc::new(Disk::default());
    let device = DevicePreferences::new(disk.clone());
    assert_eq!(device.contrast(), DEFAULT_CONTRAST);
    device.select_contrast(135).unwrap();
    assert_eq!(device.contrast(), 135);
    assert_eq!(DevicePreferences::new(disk.clone()).contrast(), 135);
    // A value edited out of range is read at its limit.
    disk.bytes.lock().unwrap()[4] = b"900".to_vec();
    assert_eq!(DevicePreferences::new(disk).contrast(), MAX_CONTRAST);
}
