use super::Failure;
use agent_protocol::providers::ProviderSettings;
use agent_protocol::session::ProviderKind;
use std::{path::PathBuf, sync::Mutex};

pub(crate) struct Settings {
    path: PathBuf,
    state: Mutex<Result<ProviderSettings, String>>,
}

impl Settings {
    pub(crate) fn new(path: PathBuf) -> Self {
        let state = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| error.to_string()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(ProviderSettings::default())
            }
            Err(error) => Err(error.to_string()),
        };
        Self {
            path,
            state: Mutex::new(state),
        }
    }

    pub(crate) fn read(&self) -> Result<ProviderSettings, Failure> {
        self.state.lock().unwrap().clone().map_err(failure)
    }

    pub(crate) fn set_enabled(
        &self,
        provider: ProviderKind,
        enabled: bool,
    ) -> Result<ProviderSettings, Failure> {
        let mut state = self.state.lock().unwrap();
        let current = state.as_ref().map_err(failure)?;
        let next = current.with_enabled(provider, enabled);
        if next != *current {
            agent_local_host::save_private_json(&self.path, &next).map_err(failure)?;
            *state = Ok(next.clone());
        }
        Ok(next)
    }
}

fn failure(error: impl std::fmt::Display) -> Failure {
    Failure::new("provider_settings_failed", error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_clients_preserve_each_others_provider_choices() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.json");
        let settings = Settings::new(path.clone());
        std::thread::scope(|scope| {
            for provider in [
                ProviderKind::Codex,
                ProviderKind::Claude,
                ProviderKind::DeepSeek,
            ] {
                let settings = &settings;
                scope.spawn(move || settings.set_enabled(provider, false).unwrap());
            }
        });
        let saved = Settings::new(path).read().unwrap();
        assert_eq!(saved.disabled.len(), 3);
        assert_eq!(saved.revision, 3);
    }

    #[test]
    fn saves_are_durable_and_failed_writes_do_not_change_live_enablement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.json");
        let settings = Settings::new(path.clone());
        settings.set_enabled(ProviderKind::Claude, false).unwrap();
        assert!(
            !Settings::new(path.clone())
                .read()
                .unwrap()
                .enabled(ProviderKind::Claude)
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(settings.set_enabled(ProviderKind::Codex, false).is_err());
        assert!(settings.read().unwrap().enabled(ProviderKind::Codex));
        std::fs::remove_dir(&path).unwrap();
        settings.set_enabled(ProviderKind::Codex, false).unwrap();
        assert_eq!(Settings::new(path).read().unwrap().disabled.len(), 2);
    }

    #[test]
    fn damaged_settings_are_reported_without_overwriting_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.json");
        std::fs::write(&path, b"damaged").unwrap();
        let settings = Settings::new(path.clone());
        assert!(settings.read().is_err());
        assert!(settings.set_enabled(ProviderKind::Codex, true).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"damaged");
    }
}
