//! Release update ownership for the Host. State transitions are kept in the
//! protocol model; this module owns only network, filesystem and archive work.
use agent_protocol::models::{
    NativeUpdatePlatform, NativeUpdateRequest, NativeUpdateState, ReleaseNoteGroup,
    UpdateActionRequest, UpdateChannel, UpdateChannelRequest, UpdateCheckRequest, UpdateState,
    UpdateStatus, UpdateTarget,
};
use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;
use ring::digest::{Context as DigestContext, SHA256};
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{fs, io::AsyncWriteExt, process::Command, sync::Mutex};
use url::Url;

const METADATA_URL_ENV: &str = "APP_RELEASE_METADATA_URL";
const CHANNEL_ENV: &str = "APP_RELEASE_CHANNEL";
const VERSION_ENV: &str = "APP_UPDATE_VERSION";
const UPDATE_TIMEOUT: Duration = Duration::from_secs(30);
const RELEASE_NOTE_GROUP_LIMIT: usize = 6;
const RELEASE_NOTE_ITEM_LIMIT: usize = 8;
const RELEASE_NOTE_LENGTH: usize = 220;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReleaseAsset {
    name: String,
    sha256: String,
    size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReleaseMetadata {
    schema: u8,
    version: String,
    channel: UpdateChannel,
    #[serde(default)]
    update_url: Option<String>,
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
    #[serde(default)]
    release_notes: Vec<ReleaseNoteGroup>,
    #[serde(default)]
    native_updates: HashMap<String, NativeReleaseLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NativeReleaseLink {
    #[serde(default)]
    url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StagedArtifact {
    path: PathBuf,
    version: String,
}

#[derive(Clone)]
pub(crate) struct UpdateManager {
    state_dir: PathBuf,
    metadata_url: Option<String>,
    default_channel: UpdateChannel,
    client: reqwest::Client,
    records: Arc<Mutex<HashMap<UpdateTarget, UpdateRecord>>>,
}

#[derive(Debug, Clone)]
struct UpdateRecord {
    state: UpdateState,
    metadata: Option<ReleaseMetadata>,
    staged: Option<StagedArtifact>,
    platform: String,
    architecture: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedUpdateRecord {
    state: UpdateState,
    metadata: Option<ReleaseMetadata>,
    staged: Option<StagedArtifact>,
    platform: String,
    architecture: String,
}

impl UpdateManager {
    pub(crate) fn new(state_dir: PathBuf) -> Self {
        let metadata_url = std::env::var(METADATA_URL_ENV)
            .ok()
            .or_else(|| option_env!("APP_RELEASE_METADATA_URL").map(str::to_owned))
            .filter(|value| !value.trim().is_empty());
        let default_channel = std::env::var(CHANNEL_ENV)
            .ok()
            .and_then(|value| match value.trim() {
                "nightly" => Some(UpdateChannel::Nightly),
                "preview" => Some(UpdateChannel::Preview),
                "stable" => Some(UpdateChannel::Stable),
                _ => None,
            })
            .or_else(|| match option_env!("APP_RELEASE_CHANNEL") {
                Some("nightly") => Some(UpdateChannel::Nightly),
                Some("preview") => Some(UpdateChannel::Preview),
                Some("stable") => Some(UpdateChannel::Stable),
                _ => None,
            })
            .unwrap_or(UpdateChannel::Stable);
        let client = reqwest::Client::builder()
            .timeout(UPDATE_TIMEOUT)
            .build()
            .expect("release update HTTP client configuration is valid");
        Self {
            state_dir,
            metadata_url,
            default_channel,
            client,
            records: Arc::default(),
        }
    }

    fn current_version() -> String {
        std::env::var(VERSION_ENV)
            .ok()
            .or_else(|| option_env!("APP_UPDATE_VERSION").map(str::to_owned))
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned())
    }

    fn platform_name(platform: &str) -> Result<&'static str> {
        match platform {
            "linux" => Ok("linux"),
            "windows" => Ok("windows"),
            "macos" | "darwin" => Ok("macos"),
            other => bail!("unsupported update platform: {other}"),
        }
    }

    fn architecture_name(architecture: &str) -> Result<&'static str> {
        match architecture {
            "x86_64" | "x64" => Ok("x86_64"),
            "arm64" | "aarch64" => Ok("arm64"),
            other => bail!("unsupported update architecture: {other}"),
        }
    }

    fn initial(&self, target: UpdateTarget) -> UpdateState {
        let enabled = self.metadata_url.is_some();
        let mut state = UpdateState::initial(
            target,
            Self::current_version(),
            self.default_channel,
            enabled,
        );
        if !enabled {
            state.message = Some(format!("{METADATA_URL_ENV} is not configured"));
        }
        state
    }

    async fn record(&self, target: UpdateTarget) -> UpdateRecord {
        {
            let records = self.records.lock().await;
            if let Some(record) = records.get(&target) {
                return record.clone();
            }
        }
        let restored = self.restore(target).await.unwrap_or_else(|| UpdateRecord {
            state: self.initial(target),
            metadata: None,
            staged: None,
            platform: std::env::consts::OS.into(),
            architecture: std::env::consts::ARCH.into(),
        });
        let mut records = self.records.lock().await;
        records.entry(target).or_insert(restored).clone()
    }

    async fn save(&self, target: UpdateTarget, record: UpdateRecord) {
        self.records.lock().await.insert(target, record.clone());
        self.persist(target, &record).await;
    }

    async fn restore(&self, target: UpdateTarget) -> Option<UpdateRecord> {
        let path = self.transaction_path(target);
        let bytes = fs::read(path).await.ok()?;
        let persisted = serde_json::from_slice::<PersistedUpdateRecord>(&bytes).ok()?;
        if let Some(staged) = &persisted.staged {
            if !staged
                .path
                .starts_with(self.state_dir.join(target_name(target)))
            {
                return None;
            }
            if fs::metadata(&staged.path).await.is_err() {
                return None;
            }
        }
        let mut state = persisted.state;
        if state.restart_required
            && state.downloaded_version.as_deref() == Some(Self::current_version().as_str())
        {
            let _ = fs::remove_file(&self.transaction_path(target)).await;
            return None;
        }
        if matches!(
            state.status,
            UpdateStatus::Checking | UpdateStatus::Downloading | UpdateStatus::Installing
        ) {
            if persisted.staged.is_some() && state.downloaded_version.is_some() {
                state.status = UpdateStatus::Downloaded;
                state.download_percent = Some(100);
                state.message = Some("Update was downloaded before the last Host shutdown".into());
                state.error_context = None;
                state.can_retry = true;
            } else {
                state.status = UpdateStatus::Error;
                state.message = Some("Update transaction was interrupted by Host shutdown".into());
                state.error_context = Some(agent_protocol::models::UpdateErrorContext::Download);
                state.can_retry = true;
            }
        }
        Some(UpdateRecord {
            state,
            metadata: persisted.metadata,
            staged: persisted.staged,
            platform: persisted.platform,
            architecture: persisted.architecture,
        })
    }

    async fn persist(&self, target: UpdateTarget, record: &UpdateRecord) {
        let directory = self.state_dir.join("transactions");
        if fs::create_dir_all(&directory).await.is_err() {
            return;
        }
        let path = self.transaction_path(target);
        let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let persisted = PersistedUpdateRecord {
            state: record.state.clone(),
            metadata: record.metadata.clone(),
            staged: record.staged.clone(),
            platform: record.platform.clone(),
            architecture: record.architecture.clone(),
        };
        let Ok(bytes) = serde_json::to_vec(&persisted) else {
            return;
        };
        if fs::write(&temporary, bytes).await.is_ok() {
            let _ = fs::remove_file(&path).await;
            let _ = fs::rename(temporary, path).await;
        }
    }

    fn transaction_path(&self, target: UpdateTarget) -> PathBuf {
        self.state_dir
            .join("transactions")
            .join(format!("{}.json", target_name(target)))
    }

    pub(crate) async fn status(
        &self,
        request: &agent_protocol::models::UpdateStatusRequest,
    ) -> UpdateState {
        self.record(request.target).await.state
    }

    pub(crate) async fn set_channel(&self, request: &UpdateChannelRequest) -> Result<UpdateState> {
        let mut record = self.record(request.target).await;
        if record.state.channel != request.channel {
            record.state = UpdateState::initial(
                request.target,
                record.state.current_version.clone(),
                request.channel,
                self.metadata_url.is_some(),
            );
            record.metadata = None;
            record.staged = None;
        }
        if self.metadata_url.is_none() {
            record.state.message = Some(format!("{METADATA_URL_ENV} is not configured"));
        }
        let state = record.state.clone();
        self.save(request.target, record).await;
        Ok(state)
    }

    pub(crate) async fn check(&self, request: &UpdateCheckRequest) -> Result<UpdateState> {
        let mut record = self.record(request.target).await;
        record.platform = request.platform.clone();
        record.architecture = request.architecture.clone();
        record.state = UpdateState {
            target: request.target,
            channel: request.channel,
            current_version: request.current_version.clone(),
            enabled: self.metadata_url.is_some(),
            ..record.state
        }
        .check_started(Some(now()));
        let Some(metadata_url) = self.metadata_url.clone() else {
            record.state = record
                .state
                .check_failed(format!("{METADATA_URL_ENV} is not configured"), Some(now()));
            record.state.status = UpdateStatus::Disabled;
            record.state.enabled = false;
            self.save(request.target, record.clone()).await;
            return Ok(record.state);
        };
        self.save(request.target, record.clone()).await;
        let result = self.fetch_metadata(&metadata_url, request.channel).await;
        let mut record = self.record(request.target).await;
        match result {
            Ok(metadata) => {
                let asset = select_asset(
                    &metadata,
                    request.target,
                    &request.platform,
                    &request.architecture,
                )?;
                let download_url = asset.as_ref().and_then(|asset| {
                    metadata
                        .update_url
                        .as_deref()
                        .and_then(|url| asset_url(url, &asset.name).ok())
                });
                let available = compare_versions(&request.current_version, &metadata.version)?
                    == Ordering::Less;
                let (release_notes, omitted_release_count) =
                    bounded_release_notes(&metadata.release_notes);
                let downloaded = record
                    .staged
                    .as_ref()
                    .is_some_and(|staged| staged.version == metadata.version);
                record.state = UpdateState {
                    status: if downloaded {
                        UpdateStatus::Downloaded
                    } else if available {
                        UpdateStatus::Available
                    } else {
                        UpdateStatus::UpToDate
                    },
                    target: request.target,
                    channel: request.channel,
                    current_version: request.current_version.clone(),
                    available_version: (available || downloaded).then(|| metadata.version.clone()),
                    downloaded_version: downloaded.then(|| metadata.version.clone()),
                    release_notes,
                    omitted_release_count,
                    download_percent: downloaded.then_some(100),
                    checked_at: Some(now()),
                    message: if available && asset.is_none() {
                        Some("No update artifact is published for this platform".into())
                    } else {
                        None
                    },
                    error_context: None,
                    can_retry: downloaded,
                    restart_required: record.state.restart_required,
                    update_url: metadata.update_url.clone(),
                    download_url,
                    artifact_name: asset.map(|asset| asset.name.clone()),
                    enabled: true,
                };
                record.metadata = Some(metadata);
            }
            Err(error) => {
                record.state = record.state.check_failed(error.to_string(), Some(now()));
            }
        }
        let state = record.state.clone();
        self.save(request.target, record).await;
        Ok(state)
    }

    pub(crate) async fn download(&self, request: &UpdateActionRequest) -> Result<UpdateState> {
        let mut record = self.record(request.target).await;
        let metadata = record
            .metadata
            .clone()
            .context("check for updates before downloading")?;
        let asset_name = record
            .state
            .artifact_name
            .clone()
            .context("no update artifact is available")?;
        let asset = metadata
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .cloned()
            .context("selected update artifact is missing")?;
        let base_url = metadata
            .update_url
            .as_deref()
            .context("release metadata has no download URL")?;
        let url = asset_url(base_url, &asset.name)?;
        record.state = record
            .state
            .download_started()
            .map_err(anyhow::Error::msg)?;
        self.save(request.target, record.clone()).await;
        let directory = self
            .state_dir
            .join(target_name(request.target))
            .join(&metadata.version);
        fs::create_dir_all(&directory).await?;
        let path = directory.join(&asset.name);
        let temporary = path.with_extension(format!(
            "{}tmp",
            path.extension()
                .and_then(|value| value.to_str())
                .unwrap_or("")
        ));
        let result = async {
            let response = self.client.get(url).send().await?;
            anyhow::ensure!(
                response.url().scheme() == "https",
                "update artifact redirect must use HTTPS"
            );
            let mut file = fs::File::create(&temporary).await?;
            let mut digest = DigestContext::new(&SHA256);
            let mut size = 0u64;
            let mut stream = response.error_for_status()?.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                size = size.saturating_add(chunk.len() as u64);
                digest.update(&chunk);
                file.write_all(&chunk).await?;
            }
            file.flush().await?;
            anyhow::ensure!(
                size == asset.size,
                "update artifact size does not match its manifest"
            );
            let digest = digest
                .finish()
                .as_ref()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            anyhow::ensure!(
                digest == asset.sha256,
                "update artifact checksum does not match its manifest"
            );
            fs::rename(&temporary, &path).await?;
            Ok::<_, anyhow::Error>(StagedArtifact {
                path,
                version: metadata.version.clone(),
            })
        }
        .await;
        if result.is_err() {
            let _ = fs::remove_file(&temporary).await;
        }
        let mut record = self.record(request.target).await;
        match result {
            Ok(staged) => {
                record.staged = Some(staged);
                record.state = record.state.download_finished(metadata.version);
            }
            Err(error) => record.state = record.state.download_failed(error.to_string()),
        }
        let state = record.state.clone();
        self.save(request.target, record).await;
        Ok(state)
    }

    pub(crate) async fn install(&self, request: &UpdateActionRequest) -> Result<UpdateState> {
        let record = self.record(request.target).await;
        let staged = record
            .staged
            .clone()
            .context("download an update before installing")?;
        let mut record = record;
        record.state = record.state.install_started().map_err(anyhow::Error::msg)?;
        self.save(request.target, record.clone()).await;
        let result = self.extract(&staged).await;
        let mut record = self.record(request.target).await;
        match result {
            Ok(()) => record.state = record.state.install_finished(),
            Err(error) => record.state = record.state.install_failed(error.to_string()),
        }
        let state = record.state.clone();
        self.save(request.target, record).await;
        Ok(state)
    }

    pub(crate) async fn native(&self, request: &NativeUpdateRequest) -> Result<NativeUpdateState> {
        let Some(metadata_url) = self.metadata_url.clone() else {
            return Ok(NativeUpdateState {
                platform: request.platform,
                channel: request.channel,
                current_version: request.current_version.clone(),
                latest_version: None,
                update_available: false,
                store_url: None,
                release_notes: vec![],
                checked_at: Some(now()),
                message: Some(format!("{METADATA_URL_ENV} is not configured")),
            });
        };
        let metadata = match self.fetch_metadata(&metadata_url, request.channel).await {
            Ok(metadata) => metadata,
            Err(error) => {
                return Ok(NativeUpdateState {
                    platform: request.platform,
                    channel: request.channel,
                    current_version: request.current_version.clone(),
                    latest_version: None,
                    update_available: false,
                    store_url: None,
                    release_notes: vec![],
                    checked_at: Some(now()),
                    message: Some(error.to_string()),
                });
            }
        };
        let key = match request.platform {
            NativeUpdatePlatform::Android => "android",
            NativeUpdatePlatform::Ios => "ios",
        };
        let store_url = metadata
            .native_updates
            .get(key)
            .and_then(|link| link.url.clone());
        let available =
            compare_versions(&request.current_version, &metadata.version)? == Ordering::Less;
        let (release_notes, _) = bounded_release_notes(&metadata.release_notes);
        Ok(NativeUpdateState {
            platform: request.platform,
            channel: request.channel,
            current_version: request.current_version.clone(),
            latest_version: Some(metadata.version),
            update_available: available && store_url.is_some(),
            store_url,
            release_notes,
            checked_at: Some(now()),
            message: (available && store_url.is_none())
                .then(|| "A newer native build exists, but no store link is configured".into()),
        })
    }

    async fn fetch_metadata(&self, url: &str, channel: UpdateChannel) -> Result<ReleaseMetadata> {
        let url = Url::parse(url).context("release metadata URL is invalid")?;
        anyhow::ensure!(
            url.scheme() == "https",
            "release metadata URL must use HTTPS"
        );
        let response = self.client.get(url).send().await?;
        anyhow::ensure!(
            response.url().scheme() == "https",
            "release metadata redirect must use HTTPS"
        );
        let metadata = response
            .error_for_status()?
            .json::<ReleaseMetadata>()
            .await?;
        anyhow::ensure!(metadata.schema == 1, "unsupported release metadata schema");
        anyhow::ensure!(
            metadata.channel == channel,
            "release metadata channel does not match the request"
        );
        parse_version(&metadata.version)?;
        if let Some(url) = &metadata.update_url {
            anyhow::ensure!(
                Url::parse(url).is_ok_and(|url| url.scheme() == "https"),
                "release update URL must use HTTPS"
            );
        }
        for asset in &metadata.assets {
            anyhow::ensure!(
                asset.name
                    == Path::new(&asset.name)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default(),
                "release asset name must not contain a path"
            );
            anyhow::ensure!(
                asset.sha256.len() == 64
                    && asset.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "release asset checksum is invalid"
            );
        }
        for link in metadata.native_updates.values() {
            if let Some(url) = &link.url {
                anyhow::ensure!(
                    Url::parse(url).is_ok_and(|url| url.scheme() == "https"),
                    "native update URL must use HTTPS"
                );
            }
        }
        Ok(metadata)
    }

    async fn extract(&self, staged: &StagedArtifact) -> Result<()> {
        let extension = if staged.path.extension().and_then(|value| value.to_str()) == Some("zip") {
            "zip"
        } else if staged.path.to_string_lossy().ends_with(".tar.gz") {
            "tar.gz"
        } else {
            bail!("unsupported update archive")
        };
        let install_root = self.state_dir.join("installed").join(&staged.version);
        let temporary = install_root.with_extension("staging");
        let _ = fs::remove_dir_all(&temporary).await;
        fs::create_dir_all(&temporary).await?;
        let listing = if extension == "zip" {
            Command::new("unzip")
                .args([
                    "-Z1",
                    staged.path.to_str().context("update path is not UTF-8")?,
                ])
                .output()
                .await?
        } else {
            Command::new("tar")
                .args([
                    "-tzf",
                    staged.path.to_str().context("update path is not UTF-8")?,
                ])
                .output()
                .await?
        };
        anyhow::ensure!(listing.status.success(), "could not inspect update archive");
        for entry in String::from_utf8_lossy(&listing.stdout).lines() {
            validate_archive_entry(entry.trim())?;
        }
        let status = if extension == "zip" {
            Command::new("unzip")
                .args([
                    "-q",
                    staged.path.to_str().context("update path is not UTF-8")?,
                    "-d",
                    temporary.to_str().context("install path is not UTF-8")?,
                ])
                .status()
                .await?
        } else {
            Command::new("tar")
                .args([
                    "--extract",
                    "--gzip",
                    "--file",
                    staged.path.to_str().context("update path is not UTF-8")?,
                    "--directory",
                    temporary.to_str().context("install path is not UTF-8")?,
                    "--no-same-owner",
                    "--no-same-permissions",
                ])
                .status()
                .await?
        };
        anyhow::ensure!(status.success(), "could not extract update archive");
        if let Some(parent) = install_root.parent() {
            fs::create_dir_all(parent).await?;
        }
        let _ = fs::remove_dir_all(&install_root).await;
        fs::rename(temporary, install_root).await?;
        Ok(())
    }
}

fn validate_archive_entry(entry: &str) -> Result<()> {
    let path = Path::new(entry);
    anyhow::ensure!(
        !path.is_absolute()
            && !path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir)),
        "update archive contains a path outside its install directory"
    );
    Ok(())
}

fn target_name(target: UpdateTarget) -> &'static str {
    match target {
        UpdateTarget::Host => "host",
        UpdateTarget::Desktop => "desktop",
    }
}

fn bounded_release_notes(notes: &[ReleaseNoteGroup]) -> (Vec<ReleaseNoteGroup>, u32) {
    let mut omitted = 0u32;
    let mut bounded = Vec::with_capacity(notes.len().min(RELEASE_NOTE_GROUP_LIMIT));
    for (group_index, group) in notes.iter().enumerate() {
        if group_index >= RELEASE_NOTE_GROUP_LIMIT {
            omitted = omitted.saturating_add(1 + group.items.len() as u32);
            continue;
        }
        let mut items = Vec::with_capacity(group.items.len().min(RELEASE_NOTE_ITEM_LIMIT));
        for (item_index, item) in group.items.iter().enumerate() {
            if item_index >= RELEASE_NOTE_ITEM_LIMIT {
                omitted = omitted.saturating_add(1);
                continue;
            }
            let mut value = item.chars().take(RELEASE_NOTE_LENGTH).collect::<String>();
            if item.chars().count() > RELEASE_NOTE_LENGTH {
                value.push('…');
            }
            if !value.trim().is_empty() {
                items.push(value);
            }
        }
        if !items.is_empty() {
            bounded.push(ReleaseNoteGroup {
                title: group.title.chars().take(120).collect::<String>(),
                items,
            });
        }
    }
    (bounded, omitted)
}

fn select_asset(
    metadata: &ReleaseMetadata,
    target: UpdateTarget,
    platform: &str,
    architecture: &str,
) -> Result<Option<ReleaseAsset>> {
    let platform = UpdateManager::platform_name(platform)?;
    let architecture = UpdateManager::architecture_name(architecture)?;
    let name = match target {
        UpdateTarget::Host => format!("host-{platform}-{architecture}.tar.gz"),
        UpdateTarget::Desktop if platform == "macos" => "desktop-macos-arm64.zip".into(),
        UpdateTarget::Desktop => format!("desktop-{platform}-{architecture}.tar.gz"),
    };
    Ok(metadata
        .assets
        .iter()
        .find(|asset| asset.name == name)
        .cloned())
}

fn asset_url(base: &str, name: &str) -> Result<String> {
    let base = Url::parse(base).context("release update URL is invalid")?;
    anyhow::ensure!(
        base.scheme() == "https",
        "release update URL must use HTTPS"
    );
    Ok(base.join(name)?.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Version<'a> {
    major: u64,
    minor: u64,
    patch: u64,
    prerelease: &'a str,
}

fn parse_version(value: &str) -> Result<Version<'_>> {
    let (core, prerelease) = value
        .split_once('-')
        .map_or((value, ""), |(core, suffix)| (core, suffix));
    let mut parts = core.split('.');
    let version = Version {
        major: parts
            .next()
            .context("release version is invalid")?
            .parse()?,
        minor: parts
            .next()
            .context("release version is invalid")?
            .parse()?,
        patch: parts
            .next()
            .context("release version is invalid")?
            .parse()?,
        prerelease,
    };
    anyhow::ensure!(parts.next().is_none(), "release version is invalid");
    Ok(version)
}

fn compare_versions(left: &str, right: &str) -> Result<Ordering> {
    let left = parse_version(left)?;
    let right = parse_version(right)?;
    for (a, b) in [
        (left.major, right.major),
        (left.minor, right.minor),
        (left.patch, right.patch),
    ] {
        if a != b {
            return Ok(a.cmp(&b));
        }
    }
    match (left.prerelease.is_empty(), right.prerelease.is_empty()) {
        (true, true) => Ok(Ordering::Equal),
        (true, false) => Ok(Ordering::Greater),
        (false, true) => Ok(Ordering::Less),
        (false, false) => Ok(left.prerelease.cmp(right.prerelease)),
    }
}

fn now() -> String {
    chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now())
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_prereleases_before_stable() {
        assert_eq!(
            compare_versions("1.0.0-nightly.2", "1.0.0-nightly.10").unwrap(),
            Ordering::Less
        );
        assert_eq!(
            compare_versions("1.0.0-nightly.10", "1.0.0").unwrap(),
            Ordering::Less
        );
    }

    #[test]
    fn rejects_archive_paths_that_escape_the_install_directory() {
        assert!(validate_archive_entry("host-daemon").is_ok());
        assert!(validate_archive_entry("nested/host-daemon").is_ok());
        assert!(validate_archive_entry("../host-daemon").is_err());
        assert!(validate_archive_entry("/tmp/host-daemon").is_err());
    }

    #[test]
    fn bounds_release_notes_for_native_consumers() {
        let notes = vec![ReleaseNoteGroup {
            title: "Changes".into(),
            items: (0..10).map(|index| format!("item-{index}")).collect(),
        }];
        let (bounded, omitted) = bounded_release_notes(&notes);
        assert_eq!(bounded[0].items.len(), RELEASE_NOTE_ITEM_LIMIT);
        assert_eq!(omitted, 2);
    }
}
