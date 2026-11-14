//! Clone jobs belong to the Host, independently of the requesting connection.
mod process;
#[cfg(test)]
mod tests;

use crate::ProjectStore;
use agent_protocol::project_clone::*;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::{Mutex, OnceCell, watch};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub(crate) struct Clones(Arc<Owner>);
struct Owner {
    projects: ProjectStore,
    registry: OnceCell<Mutex<Registry>>,
    changed: watch::Sender<Option<ProjectClones>>,
    tasks: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    state: ProjectCloneState,
    staging: Option<PathBuf>,
}
struct Registry {
    epoch: String,
    revision: u64,
    entries: Vec<Entry>,
    running: BTreeMap<String, CancellationToken>,
    shutting_down: bool,
}
impl Registry {
    fn catalog(&self) -> ProjectClones {
        ProjectClones {
            epoch: self.epoch.clone(),
            revision: self.revision,
            clones: self
                .entries
                .iter()
                .map(|entry| entry.state.clone())
                .collect(),
        }
    }
}

impl Clones {
    pub fn new(projects: ProjectStore) -> Self {
        Self(Arc::new(Owner {
            projects,
            registry: OnceCell::new(),
            changed: watch::channel(None).0,
            tasks: Default::default(),
        }))
    }
    fn path(&self) -> PathBuf {
        self.0
            .projects
            .path()
            .with_file_name("bex-project-clones.json")
    }
    pub fn subscribe(&self) -> watch::Receiver<Option<ProjectClones>> {
        self.0.changed.subscribe()
    }
    async fn registry(&self) -> Result<&Mutex<Registry>> {
        self.0
            .registry
            .get_or_try_init(|| async { self.load().await.map(Mutex::new) })
            .await
    }
    async fn load(&self) -> Result<Registry> {
        let mut entries: Vec<Entry> = match tokio::fs::read(self.path()).await {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.into()),
        };
        let mut ids = std::collections::BTreeSet::new();
        let mut interrupted = false;
        for entry in &mut entries {
            entry.state.request.validate().map_err(anyhow::Error::msg)?;
            ensure!(
                ids.insert(entry.state.request.id.clone()),
                "duplicate clone id in saved jobs"
            );
            if entry.state.phase.active() {
                interrupted = true;
                entry.state.phase = ProjectClonePhase::Failed;
                entry.state.error = Some("Host restarted during clone. Retry to continue.".into());
            }
        }
        if interrupted {
            self.persist(entries.clone()).await?;
        }
        Ok(Registry {
            epoch: uuid::Uuid::new_v4().to_string(),
            revision: 0,
            entries,
            running: BTreeMap::new(),
            shutting_down: false,
        })
    }
    async fn persist(&self, entries: Vec<Entry>) -> Result<()> {
        let path = self.path();
        tokio::task::spawn_blocking(move || agent_local_host::save_private_json(&path, &entries))
            .await??;
        Ok(())
    }
    async fn commit(
        &self,
        registry: &mut Registry,
        entries: Vec<Entry>,
        durable: bool,
    ) -> Result<()> {
        if durable {
            self.persist(entries.clone()).await?;
        }
        registry.entries = entries;
        registry.revision += 1;
        self.0.changed.send_replace(Some(registry.catalog()));
        Ok(())
    }
    pub async fn read(&self) -> Result<ProjectClones> {
        Ok(self.registry().await?.lock().await.catalog())
    }
    pub async fn start(&self, mut request: CloneProject) -> Result<ProjectClones> {
        request.validate().map_err(anyhow::Error::msg)?;
        request.destination = normalize_destination(&request.destination).await?;
        let mut registry = self.registry().await?.lock().await;
        ensure!(!registry.shutting_down, "Host is shutting down");
        if let Some(entry) = registry
            .entries
            .iter()
            .find(|entry| entry.state.request.id == request.id)
        {
            ensure!(
                entry.state.request == request,
                "clone id already belongs to another request"
            );
            return Ok(registry.catalog());
        }
        ensure!(
            registry.entries.len() < 64,
            "dismiss completed clone jobs before starting another"
        );
        ensure!(
            !registry
                .entries
                .iter()
                .any(|entry| entry.state.request.destination == request.destination),
            "destination already belongs to a clone job"
        );
        ensure!(
            tokio::fs::symlink_metadata(&request.destination)
                .await
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
            "clone destination already exists or is inaccessible"
        );
        let mut entries = registry.entries.clone();
        entries.push(Entry {
            state: ProjectCloneState {
                request: request.clone(),
                phase: ProjectClonePhase::Queued,
                progress: None,
                error: None,
            },
            staging: None,
        });
        self.commit(&mut registry, entries, true).await?;
        self.spawn(&mut registry, request);
        Ok(registry.catalog())
    }
    pub async fn change(&self, request: ChangeProjectClone) -> Result<ProjectClones> {
        validate_id(&request.id).map_err(anyhow::Error::msg)?;
        let mut registry = self.registry().await?.lock().await;
        ensure!(!registry.shutting_down, "Host is shutting down");
        let index = registry
            .entries
            .iter()
            .position(|entry| entry.state.request.id == request.id)
            .context("clone job not found")?;
        let mut entries = registry.entries.clone();
        let entry = &mut entries[index];
        match request.action {
            ProjectCloneAction::Cancel => {
                if !entry.state.phase.can_cancel() {
                    return Ok(registry.catalog());
                }
                entry.state.phase = ProjectClonePhase::Cancelling;
                self.commit(&mut registry, entries, true).await?;
                if let Some(cancel) = registry.running.get(&request.id) {
                    cancel.cancel();
                }
            }
            ProjectCloneAction::Retry => {
                ensure!(
                    entry.state.phase.can_retry(),
                    "clone job cannot be retried in this phase"
                );
                entry.state.phase = ProjectClonePhase::Queued;
                entry.state.progress = None;
                entry.state.error = None;
                let clone_request = entry.state.request.clone();
                self.commit(&mut registry, entries, true).await?;
                self.spawn(&mut registry, clone_request);
            }
            ProjectCloneAction::Dismiss => {
                ensure!(
                    !entry.state.phase.active(),
                    "cancel the clone before dismissing it"
                );
                process::cleanup(entry).await?;
                entries.remove(index);
                self.commit(&mut registry, entries, true).await?;
            }
        }
        Ok(registry.catalog())
    }
    fn spawn(&self, registry: &mut Registry, request: CloneProject) {
        let cancel = CancellationToken::new();
        registry.running.insert(request.id.clone(), cancel.clone());
        let owner = self.clone();
        let task = tokio::spawn(async move {
            owner.run(request, cancel).await;
        });
        let mut tasks = self.0.tasks.lock().unwrap();
        tasks.retain(|task| !task.is_finished());
        tasks.push(task);
    }
    async fn run(&self, request: CloneProject, cancel: CancellationToken) {
        let outcome = self.acquire(&request, &cancel).await;
        let registry = self.registry().await.expect("registry loaded before spawn");
        let mut registry = registry.lock().await;
        let mut entries = registry.entries.clone();
        let entry = entries
            .iter_mut()
            .find(|entry| entry.state.request.id == request.id)
            .unwrap();
        let cleanup = process::cleanup(entry).await;
        entry.state.phase = if outcome.is_ok() && cleanup.is_ok() {
            ProjectClonePhase::Completed
        } else if cancel.is_cancelled() {
            ProjectClonePhase::Cancelled
        } else {
            ProjectClonePhase::Failed
        };
        entry.state.progress = None;
        // Git's raw stderr can include credentials; process errors are sanitized.
        entry.state.error = outcome
            .err()
            .or_else(|| cleanup.err())
            .map(|error| error.to_string());
        let terminal = entries.clone();
        if let Err(error) = self.commit(&mut registry, entries, true).await {
            // Keep the terminal state visible even when disk failure prevents its checkpoint.
            tracing::error!(target: "agent", operation = "host.project.clone.checkpoint", %error);
            self.commit(&mut registry, terminal, false)
                .await
                .expect("memory update cannot fail");
        }
        registry.running.remove(&request.id);
    }
    async fn acquire(&self, request: &CloneProject, cancel: &CancellationToken) -> Result<()> {
        let destination = Path::new(&request.destination);
        let marker = destination.join(".git/bex-project-clone");
        if tokio::fs::read_to_string(&marker).await.ok().as_deref() != Some(&request.id) {
            let staging = {
                let mut registry = self.registry().await?.lock().await;
                let mut entries = registry.entries.clone();
                let entry = entries
                    .iter_mut()
                    .find(|entry| entry.state.request.id == request.id)
                    .unwrap();
                process::cleanup(entry).await?;
                let directory = tempfile::Builder::new().prefix(".bex-clone-").tempdir_in(
                    destination
                        .parent()
                        .context("clone destination has no parent")?,
                )?;
                tokio::fs::write(directory.path().join("owner"), &request.id).await?;
                let path = directory.path().to_path_buf();
                entry.staging = Some(path.clone());
                self.commit(&mut registry, entries, true).await?;
                let _ = directory.keep();
                path
            };
            let checkout = staging.join("checkout");
            process::clone(self, request, &checkout, cancel).await?;
            ensure!(!cancel.is_cancelled(), "Clone cancelled");
            tokio::fs::write(checkout.join(".git/bex-project-clone"), &request.id).await?;
            self.progress(&request.id, ProjectClonePhase::Registering, None)
                .await?;
            process::publish(&checkout, destination)?;
        } else {
            self.progress(&request.id, ProjectClonePhase::Registering, None)
                .await?;
        }
        self.0.projects.register(destination).await?;
        Ok(())
    }
    async fn progress(
        &self,
        id: &str,
        phase: ProjectClonePhase,
        progress: Option<CloneProgress>,
    ) -> Result<()> {
        let mut registry = self.registry().await?.lock().await;
        let mut entries = registry.entries.clone();
        let entry = entries
            .iter_mut()
            .find(|entry| entry.state.request.id == id)
            .context("clone job not found")?;
        ensure!(
            entry.state.phase != ProjectClonePhase::Cancelling,
            "Clone cancelled"
        );
        if entry.state.phase == phase && entry.state.progress == progress {
            return Ok(());
        }
        let durable = entry.state.phase != phase;
        entry.state.phase = phase;
        entry.state.progress = progress;
        self.commit(&mut registry, entries, durable).await
    }
    pub async fn shutdown(&self) {
        if let Ok(registry) = self.registry().await {
            let mut registry = registry.lock().await;
            registry.shutting_down = true;
            for cancel in registry.running.values() {
                cancel.cancel();
            }
        }
        let tasks = std::mem::take(&mut *self.0.tasks.lock().unwrap());
        for task in tasks {
            let _ = task.await;
        }
    }
}

async fn normalize_destination(destination: &str) -> Result<String> {
    let path = Path::new(destination);
    ensure!(
        path.is_absolute(),
        "destination must be absolute on this Host"
    );
    let name = path
        .file_name()
        .context("destination must name a new folder")?;
    let parent =
        tokio::fs::canonicalize(path.parent().context("destination has no parent")?).await?;
    let path = parent.join(name);
    Ok(dunce::simplified(&path)
        .to_str()
        .context("destination is not UTF-8")?
        .into())
}
