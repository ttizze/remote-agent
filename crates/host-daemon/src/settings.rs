use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use host_protocol::Ed25519PublicKey;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

const SETTINGS_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostSettings {
    pub version: u32,
    pub paired_devices: Vec<PairedDevice>,
}

impl Default for HostSettings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            paired_devices: Vec::new(),
        }
    }
}

impl HostSettings {
    pub fn upsert_paired_device(&mut self, device: PairedDevice) {
        if let Some(existing) = self
            .paired_devices
            .iter_mut()
            .find(|existing| existing.identity == device.identity)
        {
            existing.name = device.name;
        } else {
            self.paired_devices.push(device);
        }
    }

    pub fn remove_device(&mut self, identity: Ed25519PublicKey) -> bool {
        let old_len = self.paired_devices.len();
        self.paired_devices
            .retain(|device| device.identity != identity);
        self.paired_devices.len() != old_len
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedDevice {
    pub identity: Ed25519PublicKey,
    pub name: String,
}

pub fn load_settings(path: &Path) -> Result<HostSettings, SettingsError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(HostSettings::default()),
        Err(error) => return Err(SettingsError::Read(error)),
    };
    let settings: HostSettings = serde_json::from_slice(&bytes)?;
    if settings.version != SETTINGS_VERSION {
        return Err(SettingsError::UnsupportedVersion(settings.version));
    }
    Ok(settings)
}

pub fn save_settings(path: &Path, settings: &HostSettings) -> Result<(), SettingsError> {
    if settings.version != SETTINGS_VERSION {
        return Err(SettingsError::UnsupportedVersion(settings.version));
    }
    let parent = path.parent().ok_or(SettingsError::MissingParent)?;
    fs::create_dir_all(parent).map_err(SettingsError::CreateDirectory)?;
    let contents = serde_json::to_vec_pretty(settings)?;
    let temporary_path = temporary_path(path)?;

    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary_path)
            .map_err(SettingsError::CreateTemporary)?;
        file.write_all(&contents).map_err(SettingsError::Write)?;
        file.write_all(b"\n").map_err(SettingsError::Write)?;
        file.sync_all().map_err(SettingsError::Sync)?;
        fs::rename(&temporary_path, path).map_err(SettingsError::Replace)?;
        sync_directory(parent)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

fn temporary_path(path: &Path) -> Result<PathBuf, SettingsError> {
    let file_name = path.file_name().ok_or(SettingsError::MissingFileName)?;
    let mut random = [0_u8; 16];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| SettingsError::Random)?;
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let mut temporary_name = file_name.to_os_string();
    temporary_name.push(format!(".{suffix}.tmp"));
    Ok(path.with_file_name(temporary_name))
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), SettingsError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(SettingsError::SyncDirectory)
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), SettingsError> {
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("failed to read settings: {0}")]
    Read(#[source] io::Error),
    #[error("settings JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("settings version {0} is not supported")]
    UnsupportedVersion(u32),
    #[error("settings path has no parent directory")]
    MissingParent,
    #[error("settings path has no file name")]
    MissingFileName,
    #[error("failed to create settings directory: {0}")]
    CreateDirectory(#[source] io::Error),
    #[error("secure random generation failed")]
    Random,
    #[error("failed to create temporary settings file: {0}")]
    CreateTemporary(#[source] io::Error),
    #[error("failed to write settings: {0}")]
    Write(#[source] io::Error),
    #[error("failed to sync settings: {0}")]
    Sync(#[source] io::Error),
    #[error("failed to atomically replace settings: {0}")]
    Replace(#[source] io::Error),
    #[error("failed to sync settings directory: {0}")]
    SyncDirectory(#[source] io::Error),
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temporary_directory() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "remote-agent-settings-test-{}-{unique}",
            std::process::id()
        ))
    }

    #[test]
    fn missing_settings_load_as_empty_and_saves_replace_atomically() {
        let directory = temporary_directory();
        let path = directory.join("settings.json");
        assert_eq!(load_settings(&path).unwrap(), HostSettings::default());

        let device = Ed25519PublicKey::from_bytes([1; 32]);
        let mut settings = HostSettings {
            paired_devices: vec![PairedDevice {
                identity: device,
                name: "phone".to_owned(),
            }],
            ..HostSettings::default()
        };
        save_settings(&path, &settings).unwrap();
        assert_eq!(load_settings(&path).unwrap(), settings);

        settings.paired_devices[0].name = "tablet".to_owned();
        save_settings(&path, &settings).unwrap();
        assert_eq!(load_settings(&path).unwrap(), settings);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn removing_a_device_removes_only_that_device() {
        let removed = Ed25519PublicKey::from_bytes([2; 32]);
        let retained = Ed25519PublicKey::from_bytes([3; 32]);
        let mut settings = HostSettings {
            paired_devices: vec![
                PairedDevice {
                    identity: removed,
                    name: "old".to_owned(),
                },
                PairedDevice {
                    identity: retained,
                    name: "current".to_owned(),
                },
            ],
            ..HostSettings::default()
        };

        assert!(settings.remove_device(removed));
        assert_eq!(settings.paired_devices.len(), 1);
        assert_eq!(settings.paired_devices[0].identity, retained);
        assert!(!settings.remove_device(removed));
    }

    #[test]
    fn pairing_registration_updates_an_existing_device() {
        let identity = Ed25519PublicKey::from_bytes([4; 32]);
        let mut settings = HostSettings::default();
        settings.upsert_paired_device(PairedDevice {
            identity,
            name: "phone".to_owned(),
        });
        settings.upsert_paired_device(PairedDevice {
            identity,
            name: "renamed".to_owned(),
        });

        assert_eq!(settings.paired_devices.len(), 1);
        assert_eq!(settings.paired_devices[0].name, "renamed");
    }

    #[test]
    fn rejects_invalid_json_and_unknown_versions() {
        let directory = temporary_directory();
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("settings.json");
        fs::write(&path, b"not json").unwrap();
        assert!(matches!(load_settings(&path), Err(SettingsError::Json(_))));

        fs::write(&path, br#"{"version":2,"pairedDevices":[]}"#).unwrap();
        assert!(matches!(
            load_settings(&path),
            Err(SettingsError::UnsupportedVersion(2))
        ));
        fs::remove_dir_all(directory).unwrap();
    }
}
