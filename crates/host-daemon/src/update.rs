//! Release update ownership for the Host. State transitions are kept in the
//! protocol model; this module owns only network, filesystem and archive work.
use agent_protocol::models::{
    NativeUpdatePlatform, NativeUpdateRequest, NativeUpdateState, ReleaseNoteGroup,
    UpdateActionRequest, UpdateChannel, UpdateChannelRequest, UpdateCheckRequest, UpdateState,
    UpdateStatus, UpdateTarget,
};
use anyhow::{Context, Result, anyhow, bail};
use flate2::read::GzDecoder;
use futures_util::StreamExt;
use ring::digest::{Context as DigestContext, SHA256};
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    collections::HashMap,
    fs as std_fs,
    io::Read,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering as AtomicOrdering},
    },
    time::Duration,
};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{Mutex, OwnedMutexGuard},
    task,
};
use tokio_util::sync::CancellationToken;
use url::Url;

const METADATA_URL_ENV: &str = "APP_RELEASE_METADATA_URL";
const CHANNEL_ENV: &str = "APP_RELEASE_CHANNEL";
const VERSION_ENV: &str = "APP_UPDATE_VERSION";
const UPDATE_TIMEOUT: Duration = Duration::from_secs(30);
const RELEASE_NOTE_GROUP_LIMIT: usize = 6;
const RELEASE_NOTE_ITEM_LIMIT: usize = 8;
const RELEASE_NOTE_LENGTH: usize = 220;
const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ARCHIVE_UNCOMPRESSED_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_METADATA_BYTES: u64 = 1024 * 1024;
const COPY_BUFFER_SIZE: usize = 64 * 1024;
const MAX_REDIRECTS: usize = 5;
const MAX_ARCHIVE_ENTRIES: usize = 100_000;
const HANDOFF_REQUEST_FILE: &str = "update-handoff.json";

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct UpdateHandoffRequest {
    target: UpdateTarget,
}

fn handoff_request_path(state_dir: &Path) -> PathBuf {
    state_dir.join("transactions").join(HANDOFF_REQUEST_FILE)
}

/// Ask the running Host to drain before the target executable is selected.
/// The request is owner-only state and create-once so concurrent Desktop
/// launches cannot overwrite one another's handoff decision.
pub fn request_update_handoff(state_dir: &Path, target: UpdateTarget) -> Result<bool> {
    let path = handoff_request_path(state_dir);
    let parent = path.parent().context("update handoff path has no parent")?;
    crate::platform::create_state_directory(parent)?;
    let bytes = serde_json::to_vec(&UpdateHandoffRequest { target })?;
    let file = crate::platform::private_file_options().open(&path);
    let mut file = match file {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
        let _ = std_fs::remove_file(path);
        return Err(error.into());
    }
    Ok(true)
}

pub fn clear_update_handoff(state_dir: &Path) -> Result<()> {
    match std_fs::remove_file(handoff_request_path(state_dir)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn read_update_handoff(state_dir: &Path) -> Result<Option<UpdateTarget>> {
    let path = handoff_request_path(state_dir);
    let bytes = match std_fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    match serde_json::from_slice::<UpdateHandoffRequest>(&bytes) {
        Ok(request) => Ok(Some(request.target)),
        Err(_) => {
            // A torn or old request must not pin every later Host startup.
            clear_update_handoff(state_dir)?;
            Ok(None)
        }
    }
}

/// The version used by the paired Host and Desktop binaries.
pub fn current_update_version() -> String {
    std::env::var(VERSION_ENV)
        .ok()
        .or_else(|| option_env!("APP_UPDATE_VERSION").map(str::to_owned))
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned())
}

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
    size: u64,
    sha256: String,
}

#[derive(Clone)]
pub(crate) struct UpdateManager {
    state_dir: PathBuf,
    metadata_url: Option<String>,
    default_channel: UpdateChannel,
    client: reqwest::Client,
    records: Arc<Mutex<HashMap<UpdateTarget, UpdateRecord>>>,
    operation_locks: Arc<HashMap<UpdateTarget, Arc<Mutex<()>>>>,
    worker_locks: Arc<HashMap<UpdateTarget, Arc<Mutex<()>>>>,
    fences: Arc<HashMap<UpdateTarget, Arc<AtomicU64>>>,
}

struct UpdateOperation {
    _lock: OwnedMutexGuard<()>,
    worker_lock: Option<OwnedMutexGuard<()>>,
    cancellation: CancellationToken,
    generation: u64,
    manager: UpdateManager,
    target: UpdateTarget,
    fence: Arc<AtomicU64>,
    completed: bool,
}

impl UpdateOperation {
    fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    fn generation(&self) -> u64 {
        self.generation
    }

    fn fence(&self) -> Arc<AtomicU64> {
        self.fence.clone()
    }

    fn take_worker_lock(&mut self) -> OwnedMutexGuard<()> {
        self.worker_lock
            .take()
            .expect("update worker lock is available until archive extraction starts")
    }

    fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for UpdateOperation {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        self.cancellation.cancel();
        let manager = self.manager.clone();
        let target = self.target;
        let generation = self.generation;
        let lock = manager
            .operation_locks
            .get(&target)
            .expect("all update targets have an operation lock")
            .clone();
        let persist = async move {
            let _lock = lock.lock().await;
            if manager.current_generation(target) != generation {
                return;
            }
            let mut records = manager.records.lock().await;
            let Some(record) = records.get_mut(&target) else {
                return;
            };
            let status = record.state.status;
            let state = match status {
                UpdateStatus::Checking => record
                    .state
                    .check_failed("update transaction was cancelled".into(), Some(now())),
                UpdateStatus::Downloading => record
                    .state
                    .download_failed("update transaction was cancelled".into()),
                UpdateStatus::Installing => record
                    .state
                    .install_failed("update transaction was cancelled".into()),
                _ => return,
            };
            record.state = state;
            let snapshot = record.clone();
            drop(records);
            manager.persist(target, &snapshot).await;
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(persist);
        }
    }
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
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if should_follow_redirect(attempt.previous().len(), attempt.url()) {
                    attempt.follow()
                } else {
                    attempt.stop()
                }
            }))
            .build()
            .expect("release update HTTP client configuration is valid");
        let operation_locks = [
            (UpdateTarget::Host, Arc::new(Mutex::new(()))),
            (UpdateTarget::Desktop, Arc::new(Mutex::new(()))),
        ]
        .into_iter()
        .collect();
        let fences = [
            (UpdateTarget::Host, Arc::new(AtomicU64::new(0))),
            (UpdateTarget::Desktop, Arc::new(AtomicU64::new(0))),
        ]
        .into_iter()
        .collect();
        let worker_locks = [
            (UpdateTarget::Host, Arc::new(Mutex::new(()))),
            (UpdateTarget::Desktop, Arc::new(Mutex::new(()))),
        ]
        .into_iter()
        .collect();
        Self {
            state_dir,
            metadata_url,
            default_channel,
            client,
            records: Arc::default(),
            operation_locks: Arc::new(operation_locks),
            worker_locks: Arc::new(worker_locks),
            fences: Arc::new(fences),
        }
    }

    async fn begin_operation(&self, target: UpdateTarget) -> UpdateOperation {
        let lock = self
            .operation_locks
            .get(&target)
            .expect("all update targets have an operation lock")
            .clone();
        let guard = lock.lock_owned().await;
        let worker_lock = self
            .worker_locks
            .get(&target)
            .expect("all update targets have an update worker lock")
            .clone()
            .lock_owned()
            .await;
        let fence = self
            .fences
            .get(&target)
            .expect("all update targets have a generation fence")
            .clone();
        let generation = fence.fetch_add(1, AtomicOrdering::SeqCst).saturating_add(1);
        UpdateOperation {
            _lock: guard,
            worker_lock: Some(worker_lock),
            cancellation: CancellationToken::new(),
            generation,
            manager: self.clone(),
            target,
            fence,
            completed: false,
        }
    }

    fn current_generation(&self, target: UpdateTarget) -> u64 {
        self.fences
            .get(&target)
            .expect("all update targets have a generation fence")
            .load(AtomicOrdering::SeqCst)
    }

    fn current_version() -> String {
        current_update_version()
    }

    pub(crate) fn acknowledge_current(&self, target: UpdateTarget) -> Result<bool> {
        let acknowledged =
            acknowledge_update_target(&self.state_dir, target, &current_update_version())?;
        if acknowledged {
            // A target can be acknowledged before this process has loaded its
            // record. Remove a stale in-memory copy if a caller did load it.
            if let Ok(mut records) = self.records.try_lock() {
                records.remove(&target);
            }
        }
        Ok(acknowledged)
    }

    async fn accept_handoff_if_ready(&self) -> Result<bool> {
        let Some(target) = read_update_handoff(&self.state_dir)? else {
            return Ok(false);
        };
        let record = self.record(target).await;
        if !record.state.restart_required || record.state.downloaded_version.is_none() {
            clear_update_handoff(&self.state_dir)?;
            return Ok(false);
        }
        clear_update_handoff(&self.state_dir)?;
        Ok(true)
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
        if persisted.state.target != target
            || persisted.metadata.as_ref().is_some_and(|metadata| {
                Self::validate_release_metadata(metadata, None).is_err()
                    || metadata.channel != persisted.state.channel
            })
        {
            return None;
        }
        let mut state = persisted.state;
        let mut staged = persisted.staged;
        if let Some(candidate) = &staged {
            let owned = self.staged_path_is_owned(target, candidate);
            let valid = owned
                && self
                    .staged_matches_release(
                        target,
                        &state,
                        persisted.metadata.as_ref(),
                        &persisted.platform,
                        &persisted.architecture,
                        candidate,
                    )
                    .is_ok()
                && verify_staged_artifact(candidate).await.is_ok();
            if !valid {
                if owned {
                    let _ = fs::remove_file(&candidate.path).await;
                }
                staged = None;
                state.downloaded_version = None;
                state.download_percent = None;
                state.restart_required = false;
                state = state.download_failed(
                    "the staged update failed integrity verification after restart".into(),
                );
            }
        }
        if state.restart_required
            && state.downloaded_version.as_deref() == Some(Self::current_version().as_str())
        {
            if let Some(staged) = staged {
                let _ = fs::remove_file(staged.path).await;
            }
            let _ = fs::remove_file(&self.transaction_path(target)).await;
            return None;
        }
        if matches!(
            state.status,
            UpdateStatus::Checking | UpdateStatus::Downloading | UpdateStatus::Installing
        ) {
            if staged.is_some() && state.downloaded_version.is_some() {
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
            staged,
            platform: persisted.platform,
            architecture: persisted.architecture,
        })
    }

    fn staged_path_is_owned(&self, target: UpdateTarget, staged: &StagedArtifact) -> bool {
        let root = self.state_dir.join(target_name(target));
        let expected_directory = root.join(&staged.version);
        staged.path.starts_with(&expected_directory)
            && staged.path.parent() == Some(expected_directory.as_path())
            && staged.path.file_name().is_some()
            && parse_version(&staged.version).is_ok()
    }

    fn staged_matches_release(
        &self,
        target: UpdateTarget,
        state: &UpdateState,
        metadata: Option<&ReleaseMetadata>,
        platform: &str,
        architecture: &str,
        staged: &StagedArtifact,
    ) -> Result<()> {
        let metadata = metadata.context("staged update has no release metadata")?;
        anyhow::ensure!(
            state.downloaded_version.as_deref() == Some(staged.version.as_str()),
            "staged update version does not match persisted state"
        );
        anyhow::ensure!(
            staged.version == metadata.version,
            "staged update version does not match release metadata"
        );
        let asset = select_asset(metadata, target, platform, architecture)?
            .context("staged update has no matching release asset")?;
        let file_name = staged
            .path
            .file_name()
            .and_then(|value| value.to_str())
            .context("staged update path is not valid UTF-8")?;
        anyhow::ensure!(
            file_name == asset.name,
            "staged update path does not match the selected release asset"
        );
        anyhow::ensure!(
            staged.size == asset.size && staged.sha256.eq_ignore_ascii_case(&asset.sha256),
            "staged update manifest does not match the selected release asset"
        );
        Ok(())
    }

    async fn persist(&self, target: UpdateTarget, record: &UpdateRecord) {
        let directory = self.state_dir.join("transactions");
        if ensure_real_directory(&directory).await.is_err() {
            return;
        }
        let path = self.transaction_path(target);
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
        let _ = atomicwrites::AtomicFile::new(&path, atomicwrites::AllowOverwrite)
            .write(|file| file.write_all(&bytes));
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
        let mut operation = self.begin_operation(request.target).await;
        let mut record = self.record(request.target).await;
        if record.state.channel != request.channel {
            if let Some(staged) = record.staged.take() {
                let _ = fs::remove_file(staged.path).await;
            }
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
        debug_assert_eq!(
            self.current_generation(request.target),
            operation.generation()
        );
        let state = record.state.clone();
        self.save(request.target, record).await;
        operation.complete();
        Ok(state)
    }

    pub(crate) async fn check(&self, request: &UpdateCheckRequest) -> Result<UpdateState> {
        let mut operation = self.begin_operation(request.target).await;
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
            operation.complete();
            return Ok(record.state);
        };
        self.save(request.target, record.clone()).await;
        let result = self.fetch_metadata(&metadata_url, request.channel).await;
        if self.current_generation(request.target) != operation.generation() {
            return Err(anyhow!("update check was superseded"));
        }
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
                if record.state.restart_required {
                    record.metadata = Some(metadata);
                    let state = record.state.clone();
                    self.save(request.target, record).await;
                    operation.complete();
                    return Ok(state);
                }
                if record
                    .staged
                    .as_ref()
                    .is_some_and(|staged| !available || staged.version != metadata.version)
                {
                    if let Some(staged) = record.staged.take() {
                        let _ = fs::remove_file(staged.path).await;
                    }
                }
                let (release_notes, omitted_release_count) =
                    bounded_release_notes(&metadata.release_notes);
                let downloaded = record.staged.as_ref().is_some_and(|staged| {
                    available && asset.is_some() && staged.version == metadata.version
                });
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
        operation.complete();
        Ok(state)
    }

    pub(crate) async fn download(&self, request: &UpdateActionRequest) -> Result<UpdateState> {
        let mut operation = self.begin_operation(request.target).await;
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
        anyhow::ensure!(
            compare_versions(&record.state.current_version, &metadata.version)? == Ordering::Less,
            "cannot download an update that is not newer than the current version"
        );
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
        let target_directory = self.state_dir.join(target_name(request.target));
        let path = directory.join(&asset.name);
        let temporary = directory.join(format!(".{}.{}.tmp", asset.name, uuid::Uuid::new_v4()));
        ensure_real_directory(&target_directory).await?;
        ensure_real_directory(&directory).await?;
        let mut temporary_guard = TempPathGuard::empty();
        let cancellation = operation.cancellation();
        let result = async {
            let response = self.client.get(url).send().await?;
            anyhow::ensure!(
                is_https_url(response.url()),
                "update artifact redirect must use HTTPS"
            );
            let response = response.error_for_status()?;
            if let Some(length) = response.content_length() {
                anyhow::ensure!(
                    length <= asset.size && length <= MAX_DOWNLOAD_BYTES,
                    "update artifact response exceeds its manifest size"
                );
            }
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .await?;
            temporary_guard.track(temporary.clone());
            let mut digest = DigestContext::new(&SHA256);
            let mut size = 0u64;
            let mut stream = response.bytes_stream();
            loop {
                let chunk = tokio::select! {
                    _ = cancellation.cancelled() => {
                        return Err(anyhow!("update download was cancelled"));
                    }
                    chunk = stream.next() => chunk,
                };
                let Some(chunk) = chunk else { break };
                let chunk = chunk?;
                size = next_stream_size(size, chunk.len(), asset.size, MAX_DOWNLOAD_BYTES)?;
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
                digest.eq_ignore_ascii_case(&asset.sha256),
                "update artifact checksum does not match its manifest"
            );
            let _ = std_fs::remove_file(&path);
            std_fs::rename(&temporary, &path)?;
            Ok::<_, anyhow::Error>(StagedArtifact {
                path,
                version: metadata.version.clone(),
                size,
                sha256: asset.sha256.clone(),
            })
        }
        .await;
        if let Ok(staged) = &result {
            temporary_guard.track(staged.path.clone());
        }
        debug_assert_eq!(
            self.current_generation(request.target),
            operation.generation()
        );
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
        operation.complete();
        temporary_guard.disarm();
        Ok(state)
    }

    pub(crate) async fn install(&self, request: &UpdateActionRequest) -> Result<UpdateState> {
        let mut operation = self.begin_operation(request.target).await;
        let record = self.record(request.target).await;
        let staged = record
            .staged
            .clone()
            .context("download an update before installing")?;
        let mut record = record;
        let staged_path_owned = self.staged_path_is_owned(request.target, &staged);
        let validation = staged_path_owned
            .then(|| {
                self.staged_matches_release(
                    request.target,
                    &record.state,
                    record.metadata.as_ref(),
                    &record.platform,
                    &record.architecture,
                    &staged,
                )
            })
            .transpose()
            .map_err(|error| anyhow!("staged update metadata is invalid: {error}"))
            .and_then(|result| {
                result.context("staged update path is outside its target directory")
            });
        if let Err(error) = validation {
            if staged_path_owned {
                let _ = fs::remove_file(&staged.path).await;
            }
            record.staged = None;
            record.state.downloaded_version = None;
            record.state.download_percent = None;
            record.state.restart_required = false;
            record.state = record.state.download_failed(error.to_string());
            let state = record.state.clone();
            self.save(request.target, record).await;
            operation.complete();
            return Ok(state);
        }
        if let Err(error) = verify_staged_artifact(&staged).await {
            if staged_path_owned {
                let _ = fs::remove_file(&staged.path).await;
            }
            record.staged = None;
            record.state.downloaded_version = None;
            record.state.download_percent = None;
            record.state.restart_required = false;
            record.state = record.state.download_failed(error.to_string());
            let state = record.state.clone();
            self.save(request.target, record).await;
            operation.complete();
            return Ok(state);
        }
        if compare_versions(&record.state.current_version, &staged.version)? != Ordering::Less {
            let _ = fs::remove_file(&staged.path).await;
            record.staged = None;
            record.state.downloaded_version = None;
            record.state.download_percent = None;
            record.state.restart_required = false;
            record.state = record.state.download_failed(
                "cannot install an update that is not newer than the current version".into(),
            );
            let state = record.state.clone();
            self.save(request.target, record).await;
            operation.complete();
            return Ok(state);
        }
        record.state = record.state.install_started().map_err(anyhow::Error::msg)?;
        self.save(request.target, record.clone()).await;
        let result = self.extract(request.target, &staged, &mut operation).await;
        debug_assert_eq!(
            self.current_generation(request.target),
            operation.generation()
        );
        let mut record = self.record(request.target).await;
        match result {
            Ok(()) => record.state = record.state.install_finished(),
            Err(error) => record.state = record.state.install_failed(error.to_string()),
        }
        let state = record.state.clone();
        self.save(request.target, record).await;
        operation.complete();
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
        anyhow::ensure!(is_https_url(&url), "release metadata URL must use HTTPS");
        let response = self.client.get(url).send().await?;
        anyhow::ensure!(
            is_https_url(response.url()),
            "release metadata redirect must use HTTPS"
        );
        let response = response.error_for_status()?;
        let bytes = read_response_limited(response, MAX_METADATA_BYTES).await?;
        let metadata = serde_json::from_slice::<ReleaseMetadata>(&bytes)
            .context("release metadata is not valid JSON")?;
        Self::validate_release_metadata(&metadata, Some(channel))?;
        Ok(metadata)
    }

    fn validate_release_metadata(
        metadata: &ReleaseMetadata,
        requested_channel: Option<UpdateChannel>,
    ) -> Result<()> {
        anyhow::ensure!(metadata.schema == 1, "unsupported release metadata schema");
        if let Some(channel) = requested_channel {
            anyhow::ensure!(
                metadata.channel == channel,
                "release metadata channel does not match the request"
            );
        }
        parse_version(&metadata.version)?;
        if let Some(url) = &metadata.update_url {
            anyhow::ensure!(
                Url::parse(url).is_ok_and(|url| is_https_url(&url)),
                "release update URL must use HTTPS"
            );
        }
        let mut asset_names = std::collections::HashSet::new();
        for asset in &metadata.assets {
            anyhow::ensure!(
                !asset.name.is_empty()
                    && asset.name != "."
                    && asset.name != ".."
                    && asset.name
                        == Path::new(&asset.name)
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or_default()
                    && !asset.name.bytes().any(|byte| byte == b'/' || byte == b'\\'),
                "release asset name must not contain a path"
            );
            anyhow::ensure!(
                asset
                    .name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte)),
                "release asset name contains unsupported characters"
            );
            anyhow::ensure!(
                asset_names.insert(asset.name.clone()),
                "release metadata contains duplicate asset names"
            );
            anyhow::ensure!(
                asset.size <= MAX_DOWNLOAD_BYTES,
                "release asset exceeds the update size limit"
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
                    Url::parse(url).is_ok_and(|url| is_https_url(&url)),
                    "native update URL must use HTTPS"
                );
            }
        }
        Ok(())
    }

    async fn extract(
        &self,
        target: UpdateTarget,
        staged: &StagedArtifact,
        operation: &mut UpdateOperation,
    ) -> Result<()> {
        let extension = if staged.path.extension().and_then(|value| value.to_str()) == Some("zip") {
            ArchiveKind::Zip
        } else if staged
            .path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.ends_with(".tar.gz"))
        {
            ArchiveKind::TarGz
        } else {
            bail!("unsupported update archive")
        };
        let target_root = self.state_dir.join("installed").join(target_name(target));
        let install_root = target_root.join(&staged.version);
        ensure_real_directory(&target_root).await?;
        let temporary = target_root.join(format!(".staging-{}", uuid::Uuid::new_v4()));
        let archive_path = staged.path.clone();
        let cancellation = operation.cancellation();
        let fence = operation.fence();
        let generation = operation.generation();
        let worker_lock = operation.take_worker_lock();
        task::spawn_blocking(move || {
            let _worker_lock = worker_lock;
            extract_archive_sync(
                archive_path,
                extension,
                temporary,
                install_root,
                cancellation,
                fence,
                generation,
            )
        })
        .await
        .context("update archive extraction task failed")??;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
enum ArchiveKind {
    Zip,
    TarGz,
}

struct ArchiveWorkspace {
    path: Option<PathBuf>,
}

impl ArchiveWorkspace {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for ArchiveWorkspace {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = remove_existing_path(&path);
        }
    }
}

struct TempPathGuard {
    paths: Vec<PathBuf>,
}

impl TempPathGuard {
    fn empty() -> Self {
        Self { paths: Vec::new() }
    }

    fn track(&mut self, path: PathBuf) {
        self.paths.push(path);
    }

    fn disarm(&mut self) {
        self.paths.clear();
    }
}

impl Drop for TempPathGuard {
    fn drop(&mut self) {
        for path in self.paths.drain(..) {
            let _ = std_fs::remove_file(path);
        }
    }
}

fn extract_archive_sync(
    archive_path: PathBuf,
    kind: ArchiveKind,
    temporary: PathBuf,
    install_root: PathBuf,
    cancellation: CancellationToken,
    fence: Arc<AtomicU64>,
    generation: u64,
) -> Result<()> {
    std_fs::create_dir(&temporary).context("could not create update staging directory")?;
    let mut workspace = ArchiveWorkspace::new(temporary.clone());
    let marker_name = format!(".update-owner-{}", uuid::Uuid::new_v4());
    std_fs::write(temporary.join(&marker_name), generation.to_le_bytes())?;
    match kind {
        ArchiveKind::Zip => extract_zip(&archive_path, &temporary, &cancellation)?,
        ArchiveKind::TarGz => extract_tar_gz(&archive_path, &temporary, &cancellation)?,
    }
    if cancellation.is_cancelled() || fence.load(AtomicOrdering::SeqCst) != generation {
        bail!("update installation was cancelled");
    }

    let parent = install_root
        .parent()
        .context("update install root has no parent")?;
    std_fs::create_dir_all(parent)?;
    let backup = parent.join(format!(".backup-{}", uuid::Uuid::new_v4()));
    let had_previous = std_fs::symlink_metadata(&install_root).is_ok();
    if had_previous {
        std_fs::rename(&install_root, &backup).context("could not move the installed update")?;
    }
    if cancellation.is_cancelled() || fence.load(AtomicOrdering::SeqCst) != generation {
        if had_previous {
            let _ = std_fs::rename(&backup, &install_root);
        }
        bail!("update installation was cancelled");
    }
    if let Err(error) = std_fs::rename(&temporary, &install_root) {
        if had_previous {
            let _ = std_fs::rename(&backup, &install_root);
        }
        return Err(error).context("could not commit the installed update");
    }
    if cancellation.is_cancelled() || fence.load(AtomicOrdering::SeqCst) != generation {
        let committed_marker = install_root.join(&marker_name);
        if std_fs::symlink_metadata(&committed_marker).is_ok() {
            let _ = remove_existing_path(&install_root);
            if had_previous {
                let _ = std_fs::rename(&backup, &install_root);
            }
        } else if had_previous {
            if std_fs::symlink_metadata(&install_root).is_err() {
                let _ = std_fs::rename(&backup, &install_root);
            } else {
                let _ = remove_existing_path(&backup);
            }
        }
        bail!("update installation was cancelled");
    }
    let _ = std_fs::remove_file(install_root.join(&marker_name));
    workspace.disarm();
    if had_previous {
        let _ = remove_existing_path(&backup);
    }
    Ok(())
}

fn extract_tar_gz(
    archive_path: &Path,
    destination: &Path,
    cancellation: &CancellationToken,
) -> Result<()> {
    let file = std_fs::File::open(archive_path)?;
    let decoder = GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let mut total = 0u64;
    for (index, entry) in archive.entries()?.enumerate() {
        anyhow::ensure!(
            index < MAX_ARCHIVE_ENTRIES,
            "update archive contains too many entries"
        );
        let mut entry = entry?;
        if cancellation.is_cancelled() {
            bail!("update installation was cancelled");
        }
        let path = entry.path()?.into_owned();
        validate_archive_path(&path)?;
        let kind = entry.header().entry_type();
        if is_archive_root(&path) {
            anyhow::ensure!(
                kind.is_dir(),
                "update archive root entry must be a directory"
            );
        }
        anyhow::ensure!(
            kind.is_file() || kind.is_dir(),
            "update archive contains a symlink, hard link, or special file"
        );
        let declared_size = entry.header().size()?;
        if kind.is_dir() {
            anyhow::ensure!(
                declared_size == 0,
                "update archive directory has unexpected contents"
            );
            ensure_secure_directory(destination, &path)?;
            continue;
        }
        let size = declared_size;
        #[cfg(unix)]
        let mode = entry.header().mode().ok();
        ensure_total_size(total, size)?;
        let parent = path.parent().unwrap_or_else(|| Path::new(""));
        ensure_secure_directory(destination, parent)?;
        let output_path = destination.join(&path);
        let mut output = std_fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output_path)?;
        copy_archive_entry(&mut entry, &mut output, size, &mut total, cancellation)?;
        output.flush()?;
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt;
            std_fs::set_permissions(&output_path, std_fs::Permissions::from_mode(mode & 0o777))?;
        }
    }
    Ok(())
}

fn extract_zip(
    archive_path: &Path,
    destination: &Path,
    cancellation: &CancellationToken,
) -> Result<()> {
    let file = std_fs::File::open(archive_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    anyhow::ensure!(
        archive.len() <= MAX_ARCHIVE_ENTRIES,
        "update archive contains too many entries"
    );
    let mut total = 0u64;
    for index in 0..archive.len() {
        if cancellation.is_cancelled() {
            bail!("update installation was cancelled");
        }
        let mut entry = archive.by_index(index)?;
        validate_archive_path(Path::new(entry.name()))?;
        let path = entry
            .enclosed_name()
            .context("update archive contains an unsafe path")?;
        validate_archive_path(&path)?;
        anyhow::ensure!(
            !entry.is_symlink(),
            "update archive contains a symbolic link"
        );
        if entry.is_dir() {
            ensure_secure_directory(destination, &path)?;
            continue;
        }
        anyhow::ensure!(entry.is_file(), "update archive contains a special file");
        let size = entry.size();
        ensure_total_size(total, size)?;
        let parent = path.parent().unwrap_or_else(|| Path::new(""));
        ensure_secure_directory(destination, parent)?;
        let output_path = destination.join(&path);
        let mut output = std_fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output_path)?;
        copy_archive_entry(&mut entry, &mut output, size, &mut total, cancellation)?;
        output.flush()?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            std_fs::set_permissions(&output_path, std_fs::Permissions::from_mode(mode & 0o777))?;
        }
    }
    Ok(())
}

fn copy_archive_entry<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    expected_size: u64,
    total: &mut u64,
    cancellation: &CancellationToken,
) -> Result<()> {
    let mut buffer = [0u8; COPY_BUFFER_SIZE];
    let mut copied = 0u64;
    loop {
        if cancellation.is_cancelled() {
            bail!("update installation was cancelled");
        }
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let read = u64::try_from(read).context("archive entry is too large")?;
        let next = copied
            .checked_add(read)
            .context("archive entry size overflow")?;
        anyhow::ensure!(
            next <= expected_size,
            "archive entry is larger than its declared size"
        );
        let next_total = total
            .checked_add(read)
            .context("archive uncompressed size overflow")?;
        anyhow::ensure!(
            next_total <= MAX_ARCHIVE_UNCOMPRESSED_BYTES,
            "update archive exceeds the uncompressed size limit"
        );
        output.write_all(&buffer[..usize::try_from(read)?])?;
        copied = next;
        *total = next_total;
    }
    anyhow::ensure!(
        copied == expected_size,
        "archive entry is smaller than its declared size"
    );
    Ok(())
}

fn ensure_total_size(total: u64, size: u64) -> Result<()> {
    anyhow::ensure!(
        total
            .checked_add(size)
            .is_some_and(|value| value <= MAX_ARCHIVE_UNCOMPRESSED_BYTES),
        "update archive exceeds the uncompressed size limit"
    );
    Ok(())
}

fn ensure_secure_directory(root: &Path, relative: &Path) -> Result<()> {
    let root_metadata = std_fs::symlink_metadata(root)?;
    anyhow::ensure!(
        root_metadata.is_dir() && !root_metadata.file_type().is_symlink(),
        "update staging directory is not a real directory"
    );
    let mut current = root.to_path_buf();
    for component in relative.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(part) => {
                current.push(part);
                match std_fs::symlink_metadata(&current) {
                    Ok(metadata) => anyhow::ensure!(
                        metadata.is_dir() && !metadata.file_type().is_symlink(),
                        "update archive path traverses a non-directory"
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        std_fs::create_dir(&current)?;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            _ => bail!("update archive contains an unsafe path"),
        }
    }
    Ok(())
}

fn remove_existing_path(path: &Path) -> std::io::Result<()> {
    match std_fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            std_fs::remove_dir_all(path)
        }
        Ok(_) => std_fs::remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn validate_archive_entry(entry: &str) -> Result<()> {
    validate_archive_path(Path::new(entry))
}

fn validate_archive_path(path: &Path) -> Result<()> {
    let mut has_normal_component = false;
    let mut has_component = false;
    for component in path.components() {
        has_component = true;
        match component {
            std::path::Component::Normal(_) => has_normal_component = true,
            std::path::Component::CurDir => {}
            std::path::Component::Prefix(..)
            | std::path::Component::RootDir
            | std::path::Component::ParentDir => {
                bail!("update archive contains a path outside its install directory")
            }
        }
    }
    anyhow::ensure!(
        has_normal_component || has_component && is_archive_root(path),
        "update archive contains an empty path"
    );
    Ok(())
}

fn is_archive_root(path: &Path) -> bool {
    path.components()
        .all(|component| matches!(component, std::path::Component::CurDir))
}

fn is_https_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
}

fn should_follow_redirect(previous_count: usize, next: &Url) -> bool {
    previous_count <= MAX_REDIRECTS && is_https_url(next)
}

async fn read_response_limited(response: reqwest::Response, maximum: u64) -> Result<Vec<u8>> {
    if let Some(length) = response.content_length() {
        anyhow::ensure!(length <= maximum, "update response exceeds its size limit");
    }
    let mut body = Vec::new();
    let mut size = 0u64;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let next_size = next_stream_size(size, chunk.len(), maximum, maximum)?;
        body.extend_from_slice(&chunk);
        size = next_size;
    }
    Ok(body)
}

fn next_stream_size(current: u64, chunk_size: usize, expected: u64, maximum: u64) -> Result<u64> {
    let chunk_size = u64::try_from(chunk_size).context("update response chunk is too large")?;
    let next_size = current
        .checked_add(chunk_size)
        .context("update response size overflow")?;
    anyhow::ensure!(
        next_size <= expected && next_size <= maximum,
        "update response exceeds its size limit"
    );
    Ok(next_size)
}

async fn verify_staged_artifact(staged: &StagedArtifact) -> Result<()> {
    parse_version(&staged.version)?;
    anyhow::ensure!(
        staged.size <= MAX_DOWNLOAD_BYTES,
        "staged update exceeds the download size limit"
    );
    let mut parent = staged.path.parent();
    while let Some(path) = parent {
        let metadata = fs::symlink_metadata(path).await?;
        anyhow::ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "staged update path traverses a symlink"
        );
        let next = path.parent();
        if next == Some(path) {
            break;
        }
        parent = next;
    }
    let metadata = fs::symlink_metadata(&staged.path).await?;
    anyhow::ensure!(
        metadata.file_type().is_file(),
        "staged update is not a regular file"
    );
    anyhow::ensure!(
        metadata.len() == staged.size,
        "staged update size does not match its manifest"
    );
    let mut file = fs::File::open(&staged.path).await?;
    let mut digest = DigestContext::new(&SHA256);
    let mut buffer = [0u8; COPY_BUFFER_SIZE];
    let mut size = 0u64;
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let read = u64::try_from(read).context("staged update is too large")?;
        size = size
            .checked_add(read)
            .context("staged update size overflow")?;
        anyhow::ensure!(
            size <= staged.size && size <= MAX_DOWNLOAD_BYTES,
            "staged update exceeds its manifest size"
        );
        digest.update(&buffer[..usize::try_from(read)?]);
    }
    anyhow::ensure!(
        size == staged.size,
        "staged update size does not match its manifest"
    );
    let actual = digest
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    anyhow::ensure!(
        actual.eq_ignore_ascii_case(&staged.sha256),
        "staged update checksum does not match its manifest"
    );
    Ok(())
}

async fn ensure_real_directory(path: &Path) -> Result<()> {
    anyhow::ensure!(!path.as_os_str().is_empty(), "update path is empty");
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => continue,
            std::path::Component::ParentDir => {
                bail!("update path contains a parent directory component")
            }
            std::path::Component::Prefix(..) | std::path::Component::RootDir => {
                current.push(component.as_os_str());
            }
            std::path::Component::Normal(part) => current.push(part),
        }
        match fs::symlink_metadata(&current).await {
            Ok(metadata) => anyhow::ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "update path traverses a symlink or non-directory"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current).await?;
                let metadata = fs::symlink_metadata(&current).await?;
                anyhow::ensure!(
                    metadata.is_dir() && !metadata.file_type().is_symlink(),
                    "update path is not a real directory"
                );
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn target_name(target: UpdateTarget) -> &'static str {
    match target {
        UpdateTarget::Host => "host",
        UpdateTarget::Desktop => "desktop",
    }
}

/// Clear a completed install only when the target process is running the
/// exact version that was installed. The target calls this after it has
/// crossed its own launch boundary; the shared Host does not infer Desktop
/// success from merely observing a transaction.
pub fn acknowledge_update_target(
    state_dir: &Path,
    target: UpdateTarget,
    current_version: &str,
) -> Result<bool> {
    let path = state_dir
        .join("transactions")
        .join(format!("{}.json", target_name(target)));
    let bytes = match std_fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let persisted: PersistedUpdateRecord = match serde_json::from_slice(&bytes) {
        Ok(persisted) => persisted,
        Err(_) => return Ok(false),
    };
    if persisted.state.target != target
        || !persisted.state.restart_required
        || persisted.state.downloaded_version.as_deref() != Some(current_version)
    {
        return Ok(false);
    }
    if let Some(staged) = persisted.staged
        && staged_path_is_owned(state_dir, target, &staged)
    {
        let _ = std_fs::remove_file(staged.path);
    }
    match std_fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn staged_path_is_owned(state_dir: &Path, target: UpdateTarget, staged: &StagedArtifact) -> bool {
    let expected_directory = state_dir.join(target_name(target)).join(&staged.version);
    staged.path.starts_with(&expected_directory)
        && staged.path.parent() == Some(expected_directory.as_path())
        && staged.path.file_name().is_some()
        && parse_version(&staged.version).is_ok()
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
    anyhow::ensure!(is_https_url(&base), "release update URL must use HTTPS");
    Ok(base.join(name)?.to_string())
}

fn parse_version(value: &str) -> Result<semver::Version> {
    semver::Version::parse(value).map_err(|error| anyhow!("release version is invalid: {error}"))
}

fn compare_versions(left: &str, right: &str) -> Result<Ordering> {
    Ok(parse_version(left)?.cmp(&parse_version(right)?))
}

fn now() -> String {
    chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now())
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn tar_gz_entry(
        path: &str,
        entry_type: tar::EntryType,
        data: &[u8],
        link_name: Option<&str>,
    ) -> Vec<u8> {
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_path(path).unwrap();
        header.set_entry_type(entry_type);
        header.set_size(if entry_type.is_file() {
            data.len() as u64
        } else {
            0
        });
        if let Some(link_name) = link_name {
            header.set_link_name(link_name).unwrap();
        }
        header.set_cksum();
        builder.append(&header, data).unwrap();
        builder.into_inner().unwrap().finish().unwrap()
    }

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
    fn strict_versions_cannot_be_used_for_path_traversal() {
        assert!(parse_version("1.2.3").is_ok());
        assert!(parse_version("1.2.3-nightly.10").is_ok());
        assert!(parse_version("1.2.3/../../outside").is_err());
        assert!(parse_version("1.2.3-../../outside").is_err());
        assert!(parse_version("1.2").is_err());
    }

    #[test]
    fn update_urls_require_https_and_a_host() {
        assert!(is_https_url(
            &Url::parse("https://updates.example.test/releases").unwrap()
        ));
        assert!(!is_https_url(
            &Url::parse("http://updates.example.test/releases").unwrap()
        ));
        let mut no_host = Url::parse("https://updates.example.test/releases").unwrap();
        no_host.set_host(None).unwrap();
        assert!(!is_https_url(&no_host));
        assert!(!is_https_url(
            &Url::parse("https://user:password@updates.example.test/releases").unwrap()
        ));
    }

    #[test]
    fn redirect_policy_requires_https_and_has_a_bounded_chain() {
        let https = Url::parse("https://updates.example.test/releases").unwrap();
        let http = Url::parse("http://updates.example.test/releases").unwrap();
        assert!(should_follow_redirect(1, &https));
        assert!(should_follow_redirect(MAX_REDIRECTS, &https));
        assert!(!should_follow_redirect(MAX_REDIRECTS + 1, &https));
        assert!(!should_follow_redirect(1, &http));
    }

    #[test]
    fn release_assets_are_unique_and_safe_path_segments() {
        let asset = ReleaseAsset {
            name: "host-linux-x86_64.tar.gz".into(),
            sha256: "0".repeat(64),
            size: 1,
        };
        let metadata = ReleaseMetadata {
            schema: 1,
            version: "1.2.3".into(),
            channel: UpdateChannel::Nightly,
            update_url: Some("https://updates.example.test/".into()),
            assets: vec![asset.clone()],
            release_notes: vec![],
            native_updates: HashMap::new(),
        };
        assert!(UpdateManager::validate_release_metadata(&metadata, None).is_ok());
        let mut duplicate = metadata.clone();
        duplicate.assets.push(asset);
        assert!(UpdateManager::validate_release_metadata(&duplicate, None).is_err());
        let mut unsafe_name = metadata;
        unsafe_name.assets[0].name = "host-linux-x86_64.tar.gz?redirect".into();
        assert!(UpdateManager::validate_release_metadata(&unsafe_name, None).is_err());
    }

    #[test]
    fn rejects_archive_paths_that_escape_the_install_directory() {
        assert!(validate_archive_entry("host-daemon").is_ok());
        assert!(validate_archive_entry("nested/host-daemon").is_ok());
        assert!(validate_archive_entry("./nested/host-daemon").is_ok());
        assert!(validate_archive_entry(".").is_ok());
        assert!(validate_archive_entry("").is_err());
        assert!(validate_archive_entry("nested/../host-daemon").is_err());
        assert!(validate_archive_entry("../host-daemon").is_err());
        assert!(validate_archive_entry("/tmp/host-daemon").is_err());
    }

    #[test]
    fn extracts_tar_archive_root_directory_entry() {
        let directory = tempfile::tempdir().unwrap();
        let archive_path = directory.path().join("update.tar.gz");
        std::fs::write(
            &archive_path,
            tar_gz_entry(".", tar::EntryType::dir(), &[], None),
        )
        .unwrap();
        let staging = directory.path().join("staging");
        let install = directory.path().join("installed");
        extract_archive_sync(
            archive_path,
            ArchiveKind::TarGz,
            staging,
            install.clone(),
            CancellationToken::new(),
            Arc::new(AtomicU64::new(1)),
            1,
        )
        .unwrap();
        assert!(install.is_dir());
    }

    #[test]
    fn rejects_symlink_and_hardlink_tar_entries_and_cleans_staging() {
        let directory = tempfile::tempdir().unwrap();
        for entry_type in [tar::EntryType::symlink(), tar::EntryType::hard_link()] {
            let archive_path = directory.path().join("update.tar.gz");
            std::fs::write(
                &archive_path,
                tar_gz_entry("host-daemon", entry_type, &[], Some("/etc/passwd")),
            )
            .unwrap();
            let staging = directory
                .path()
                .join(format!("staging-{}", entry_type.as_byte()));
            let install = directory.path().join("installed");
            let result = extract_archive_sync(
                archive_path,
                ArchiveKind::TarGz,
                staging.clone(),
                install,
                CancellationToken::new(),
                Arc::new(AtomicU64::new(1)),
                1,
            );
            assert!(result.is_err());
            assert!(!staging.exists());
        }
    }

    #[test]
    fn rejects_tar_path_escape_and_keeps_install_root_absent() {
        let directory = tempfile::tempdir().unwrap();
        let archive_path = directory.path().join("update.tar.gz");
        std::fs::write(
            &archive_path,
            tar_gz_entry("../outside", tar::EntryType::file(), b"unsafe", None),
        )
        .unwrap();
        let staging = directory.path().join("staging");
        let install = directory.path().join("installed");
        let result = extract_archive_sync(
            archive_path,
            ArchiveKind::TarGz,
            staging.clone(),
            install.clone(),
            CancellationToken::new(),
            Arc::new(AtomicU64::new(1)),
            1,
        );
        assert!(result.is_err());
        assert!(!staging.exists());
        assert!(!install.exists());
    }

    #[test]
    fn bounded_archive_copy_rejects_before_writing() {
        let mut input = Cursor::new(vec![1u8, 2, 3]);
        let mut output = Vec::new();
        let mut total = MAX_ARCHIVE_UNCOMPRESSED_BYTES;
        let result = copy_archive_entry(
            &mut input,
            &mut output,
            3,
            &mut total,
            &CancellationToken::new(),
        );
        assert!(result.is_err());
        assert!(output.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn directory_creation_rejects_symlink_ancestors_before_writing() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let outside = directory.path().join("outside");
        let link = directory.path().join("link");
        std::fs::create_dir(&outside).unwrap();
        symlink(&outside, &link).unwrap();
        let target = link.join("created");
        assert!(ensure_real_directory(&target).await.is_err());
        assert!(!outside.join("created").exists());
    }

    #[test]
    fn download_chunk_limit_is_checked_before_write() {
        assert_eq!(next_stream_size(2, 3, 5, 10).unwrap(), 5);
        assert!(next_stream_size(2, 4, 5, 10).is_err());
        assert!(next_stream_size(9, 2, 20, 10).is_err());
    }

    #[test]
    fn cancelled_archive_extraction_cleans_staging() {
        let directory = tempfile::tempdir().unwrap();
        let archive_path = directory.path().join("update.tar.gz");
        std::fs::write(
            &archive_path,
            tar_gz_entry("host-daemon", tar::EntryType::file(), b"binary", None),
        )
        .unwrap();
        let staging = directory.path().join("staging");
        let token = CancellationToken::new();
        token.cancel();
        let result = extract_archive_sync(
            archive_path,
            ArchiveKind::TarGz,
            staging.clone(),
            directory.path().join("installed"),
            token,
            Arc::new(AtomicU64::new(1)),
            1,
        );
        assert!(result.is_err());
        assert!(!staging.exists());
    }

    #[tokio::test]
    async fn target_operations_advance_generation_after_serial_completion() {
        let directory = tempfile::tempdir().unwrap();
        let manager = UpdateManager::new(directory.path().to_path_buf());
        let first = manager.begin_operation(UpdateTarget::Host).await;
        let first_generation = first.generation();
        drop(first);
        let second = manager.begin_operation(UpdateTarget::Host).await;
        assert!(second.generation() > first_generation);
    }

    #[test]
    fn stale_archive_completion_cannot_commit_a_newer_generation() {
        let directory = tempfile::tempdir().unwrap();
        let archive_path = directory.path().join("update.tar.gz");
        std::fs::write(
            &archive_path,
            tar_gz_entry("host-daemon", tar::EntryType::file(), b"binary", None),
        )
        .unwrap();
        let staging = directory.path().join("staging");
        let install = directory.path().join("installed");
        let fence = Arc::new(AtomicU64::new(2));
        let result = extract_archive_sync(
            archive_path,
            ArchiveKind::TarGz,
            staging.clone(),
            install,
            CancellationToken::new(),
            fence,
            1,
        );
        assert!(result.is_err());
        assert!(!staging.exists());
    }

    #[tokio::test]
    async fn staged_artifact_checksum_is_reverified_before_use() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("host")
            .join("1.0.0")
            .join("host.tar.gz");
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&path, b"good").await.unwrap();
        let mut digest = DigestContext::new(&SHA256);
        digest.update(b"good");
        let checksum = digest
            .finish()
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let staged = StagedArtifact {
            path: path.clone(),
            version: "1.0.0".into(),
            size: 4,
            sha256: checksum,
        };
        verify_staged_artifact(&staged).await.unwrap();
        tokio::fs::write(&path, b"evil").await.unwrap();
        assert!(verify_staged_artifact(&staged).await.is_err());
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

    #[test]
    fn handoff_request_is_single_writer_and_clearable() {
        let directory = tempfile::tempdir().unwrap();
        assert!(request_update_handoff(directory.path(), UpdateTarget::Desktop).unwrap());
        assert!(!request_update_handoff(directory.path(), UpdateTarget::Host).unwrap());
        let request: UpdateHandoffRequest =
            serde_json::from_slice(&std::fs::read(handoff_request_path(directory.path())).unwrap())
                .unwrap();
        assert_eq!(request.target, UpdateTarget::Desktop);
        clear_update_handoff(directory.path()).unwrap();
        assert!(!handoff_request_path(directory.path()).exists());
    }

    #[tokio::test]
    async fn handoff_is_consumed_only_for_a_restartable_install() {
        let directory = tempfile::tempdir().unwrap();
        let manager = UpdateManager::new(directory.path().to_path_buf());
        request_update_handoff(directory.path(), UpdateTarget::Desktop).unwrap();
        assert!(!manager.accept_handoff_if_ready().await.unwrap());
        assert!(!handoff_request_path(directory.path()).exists());

        let mut state = UpdateState::initial(
            UpdateTarget::Desktop,
            "1.0.0".into(),
            UpdateChannel::Nightly,
            true,
        );
        state.status = UpdateStatus::Downloaded;
        state.downloaded_version = Some("1.1.0".into());
        state.restart_required = true;
        let transaction = PersistedUpdateRecord {
            state,
            metadata: None,
            staged: None,
            platform: "linux".into(),
            architecture: "x86_64".into(),
        };
        let path = directory.path().join("transactions/desktop.json");
        std::fs::write(&path, serde_json::to_vec(&transaction).unwrap()).unwrap();
        let manager = UpdateManager::new(directory.path().to_path_buf());
        request_update_handoff(directory.path(), UpdateTarget::Desktop).unwrap();
        assert!(manager.accept_handoff_if_ready().await.unwrap());
        assert!(!handoff_request_path(directory.path()).exists());
    }

    #[test]
    fn target_acknowledgement_requires_the_exact_running_version() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("transactions/host.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut state = UpdateState::initial(
            UpdateTarget::Host,
            "1.0.0".into(),
            UpdateChannel::Nightly,
            true,
        );
        state.status = UpdateStatus::Downloaded;
        state.downloaded_version = Some("1.1.0".into());
        state.restart_required = true;
        let persisted = PersistedUpdateRecord {
            state,
            metadata: None,
            staged: None,
            platform: "linux".into(),
            architecture: "x86_64".into(),
        };
        std::fs::write(&path, serde_json::to_vec(&persisted).unwrap()).unwrap();
        assert!(!acknowledge_update_target(directory.path(), UpdateTarget::Host, "1.0.0").unwrap());
        assert!(path.exists());
        assert!(acknowledge_update_target(directory.path(), UpdateTarget::Host, "1.1.0").unwrap());
        assert!(!path.exists());
    }
}
