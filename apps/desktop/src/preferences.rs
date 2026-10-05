use agent_core::{persistence::ClientPreferences, store::DevicePreferences};
use std::{path::PathBuf, sync::Arc};

/// Owns the single device preference file and orders all window flushes.
pub(crate) struct Preferences {
    pub(crate) owner: Arc<DevicePreferences>,
    path: PathBuf,
    saved: tokio::sync::Mutex<Option<ClientPreferences>>,
}
impl Preferences {
    pub(crate) async fn load(path: PathBuf) -> anyhow::Result<Self> {
        let saved = match tokio::fs::read(&path).await {
            Ok(bytes) => Some(serde_json::from_slice::<ClientPreferences>(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            owner: Arc::new(DevicePreferences::new(saved.clone().unwrap_or_default())),
            path,
            saved: tokio::sync::Mutex::new(saved),
        })
    }

    pub(crate) async fn save(&self) -> anyhow::Result<()> {
        let mut saved = self.saved.lock().await;
        let current = self.owner.capture();
        if saved.as_ref() != Some(&current) {
            let path = self.path.clone();
            let writing = current.clone();
            tokio::task::spawn_blocking(move || {
                host_daemon::platform::save_private_json(&path, &writing)
            })
            .await??;
            *saved = Some(current);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::{
        state::{FollowUpBehavior, Intent, ModelDefaultsScope, Snapshot},
        store::Store,
    };

    #[tokio::test]
    async fn stale_window_flush_cannot_overwrite_new_device_preferences() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let device = Arc::new(Preferences::load(path.clone()).await.unwrap());
        let main = Arc::new(Store::offline(Snapshot::default()));
        let side = Arc::new(Store::offline(Snapshot::default()));
        device.owner.attach(&main);
        device.owner.attach(&side);
        main.dispatch(Intent::SetFollowUpBehavior {
            behavior: FollowUpBehavior::Steer,
        })
        .await
        .unwrap();
        device.save().await.unwrap();
        main.close().await.unwrap();
        side.dispatch(Intent::SelectDefaultModelOption {
            scope: ModelDefaultsScope::Global,
            id: "reasoningEffort".into(),
            value: Some(agent_protocol::models::ModelOptionValue::String(
                "high".into(),
            )),
        })
        .await
        .unwrap();
        let (a, b) = tokio::join!(device.save(), device.save());
        a.unwrap();
        b.unwrap();
        let restarted = Preferences::load(path).await.unwrap();
        let mut snapshot = Snapshot::default();
        restarted.owner.capture().apply(&mut snapshot);
        assert_eq!(snapshot.follow_up_behavior, FollowUpBehavior::Steer);
        assert_eq!(
            agent_protocol::models::model_option_string(
                &snapshot.model_defaults.options,
                "reasoningEffort"
            ),
            Some("high")
        );
    }
}
