use std::{
    env, io,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use agent_core::models::Thread;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::io::AsyncReadExt;

pub(crate) mod state;
pub(crate) mod titles;

pub const HOST_THREAD_LIST_METHOD: &str = "host/thread/list";
pub const HOST_THREAD_START_METHOD: &str = "host/thread/start";
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadPage {
    pub data: Vec<Thread>,
    pub next_cursor: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

const MAX_STATE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct DesktopProjectStore {
    path: PathBuf,
    cache: Arc<tokio::sync::Mutex<Option<CachedSnapshot>>>,
}

#[derive(Debug)]
struct CachedSnapshot {
    sources: [Option<FileStamp>; 3],
    snapshot: Arc<state::Snapshot>,
}

#[derive(Debug, PartialEq)]
struct FileStamp {
    modified: SystemTime,
    length: u64,
    created: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}
async fn file_stamp(path: &Path) -> Result<Option<FileStamp>, DesktopProjectError> {
    let metadata = match tokio::fs::metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(DesktopProjectError::Read(error)),
    };
    Ok(Some(FileStamp {
        modified: metadata.modified().map_err(DesktopProjectError::Read)?,
        length: metadata.len(),
        created: metadata.created().ok(),
        #[cfg(unix)]
        identity: {
            use std::os::unix::fs::MetadataExt;
            (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            )
        },
    }))
}

impl DesktopProjectStore {
    pub fn from_environment() -> Result<Self, DesktopProjectError> {
        let codex_home = env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".codex")));
        let codex_home = codex_home.ok_or(DesktopProjectError::MissingHome)?;
        Ok(Self::new(codex_home.join(".codex-global-state.json")))
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            cache: Arc::default(),
        }
    }

    pub async fn enrich_threads(&self, threads: &mut [Thread]) -> Result<(), DesktopProjectError> {
        let snapshot = self.load().await?;
        for thread in threads {
            snapshot.enrich_thread(thread);
        }
        Ok(())
    }

    async fn source_stamps(&self) -> Result<[Option<FileStamp>; 3], DesktopProjectError> {
        Ok([
            file_stamp(&self.path).await?,
            file_stamp(&self.path.with_file_name("bex-worktrees.json")).await?,
            file_stamp(&self.chat_directory()).await?,
        ])
    }

    pub(crate) async fn load(&self) -> Result<Arc<state::Snapshot>, DesktopProjectError> {
        let mut cache = self.cache.lock().await;
        let sources = self.source_stamps().await?;
        if let Some(cached) = &*cache
            && cached.sources == sources
        {
            return Ok(cached.snapshot.clone());
        }
        let snapshot = Arc::new(self.load_uncached().await?);
        // An in-place write or atomic replacement during the read must not
        // associate the resulting snapshot with a different file version.
        *cache = if self.source_stamps().await? == sources {
            Some(CachedSnapshot {
                sources,
                snapshot: snapshot.clone(),
            })
        } else {
            None
        };
        Ok(snapshot)
    }

    pub(crate) fn chat_directory(&self) -> PathBuf {
        self.path.with_file_name("bex-chats")
    }

    async fn load_uncached(&self) -> Result<state::Snapshot, DesktopProjectError> {
        let mut snapshot = self.read_desktop_state().await?;
        for root in snapshot.projects.iter().flat_map(|project| &project.roots) {
            if !Path::new(&root.path).is_absolute() {
                continue;
            }
            match tokio::fs::canonicalize(&root.path).await {
                Ok(path) => {
                    snapshot.resolved_roots.insert(root.path.clone(), path);
                }
                // Saved projects remain listable when their directories are offline or inaccessible.
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                    ) => {}
                Err(error) => return Err(DesktopProjectError::Read(error)),
            }
        }
        snapshot.worktree_roots = crate::worktrees::workspace_roots(&self.path)
            .await
            .map_err(|error| DesktopProjectError::Read(io::Error::other(error)))?;
        snapshot.chat_directory = match tokio::fs::canonicalize(self.chat_directory()).await {
            Ok(path) => Some(path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(DesktopProjectError::Read(error)),
        };
        Ok(snapshot)
    }

    async fn read_desktop_state(&self) -> Result<state::Snapshot, DesktopProjectError> {
        let file = match tokio::fs::File::open(&self.path).await {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(state::Snapshot::default());
            }
            Err(error) => return Err(DesktopProjectError::Read(error)),
        };
        let metadata = file.metadata().await.map_err(DesktopProjectError::Read)?;
        if metadata.len() > MAX_STATE_BYTES {
            return Err(DesktopProjectError::TooLarge(metadata.len()));
        }

        let mut bytes = Vec::new();
        let bytes_read = file
            .take(MAX_STATE_BYTES + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(DesktopProjectError::Read)?;
        if bytes_read > MAX_STATE_BYTES as usize {
            return Err(DesktopProjectError::TooLarge(bytes_read as u64));
        }
        tokio::task::spawn_blocking(move || state::Snapshot::parse(&bytes))
            .await
            .map_err(|error| DesktopProjectError::Read(io::Error::other(error)))?
            .map_err(DesktopProjectError::Invalid)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DesktopProjectError {
    #[error("neither CODEX_HOME nor the user home directory is available")]
    MissingHome,
    #[error("failed to read Codex Desktop project state: {0}")]
    Read(#[source] io::Error),
    #[error("Codex Desktop project state is too large ({0} bytes)")]
    TooLarge(u64),
    #[error("invalid Codex Desktop project state: {0}")]
    Invalid(#[source] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use serde_json::json;

    fn temporary_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is before the Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "remote-agent-desktop-projects-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn chat_scope_survives_missing_desktop_state_and_workspace_matching() {
        let directory = tempfile::tempdir().unwrap();
        let store = DesktopProjectStore::new(directory.path().join("projects.json"));
        let missing = store.load().await.unwrap();
        assert!(missing.chat_directory.is_none());
        assert!(missing.projects.is_empty());
        tokio::fs::create_dir(store.chat_directory()).await.unwrap();
        let cwd = tokio::fs::canonicalize(store.chat_directory())
            .await
            .unwrap();
        for state in [
            None,
            Some(json!({
                "local-projects":{"parent":{"id":"parent","name":"Parent","rootPaths":[directory.path().canonicalize().unwrap()]}},
                "thread-project-assignments":{"assigned":{"projectId":"parent"}}
            })),
        ] {
            if let Some(state) = state {
                tokio::fs::write(store.path(), serde_json::to_vec(&state).unwrap())
                    .await
                    .unwrap();
            }
            let mut chat: Thread = serde_json::from_value(json!({"id":"chat","cwd":cwd})).unwrap();
            store
                .enrich_threads(std::slice::from_mut(&mut chat))
                .await
                .unwrap();
            assert_eq!(chat.project_id, Some(None));
        }
        let mut assigned: Thread =
            serde_json::from_value(json!({"id":"assigned","cwd":cwd})).unwrap();
        store
            .enrich_threads(std::slice::from_mut(&mut assigned))
            .await
            .unwrap();
        assert_eq!(assigned.project_id, Some(Some("parent".into())));
    }

    #[tokio::test]
    async fn oversized_state_file_reports_its_size() {
        let path = temporary_path("too-large");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_STATE_BYTES + 2).unwrap();
        drop(file);

        let error = DesktopProjectStore::new(&path).load().await.unwrap_err();
        let _ = std::fs::remove_file(path);

        assert!(matches!(
            error,
            DesktopProjectError::TooLarge(bytes) if bytes == MAX_STATE_BYTES + 2
        ));
    }
    #[tokio::test]
    async fn cache_tracks_replacements_deletion_corruption_and_worktree_membership() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("projects.json");
        std::fs::write(
            &path,
            r#"{"local-projects":{"p":{"id":"p","name":"first","rootPaths":["/repo"]}}}"#,
        )
        .unwrap();
        let store = DesktopProjectStore::new(&path);
        let first = store.load().await.unwrap();
        assert!(Arc::ptr_eq(&first, &store.clone().load().await.unwrap()));
        let previous_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let replacement = directory.path().join("replacement");
        std::fs::write(
            &replacement,
            r#"{"local-projects":{"p":{"id":"p","name":"other","rootPaths":["/repo"]}}}"#,
        )
        .unwrap();
        std::fs::File::options()
            .write(true)
            .open(&replacement)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(previous_mtime))
            .unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        let replaced = store.load().await.unwrap();
        assert_eq!(replaced.projects[0].name, "other");
        assert!(!Arc::ptr_eq(&first, &replaced));
        let roots = directory.path().join("bex-worktrees.json");
        std::fs::write(&roots, r#"{"workspaceRoots":{"/session":"/repo"}}"#).unwrap();
        let mut thread = Thread {
            id: Some("thread".into()),
            cwd: Some("/session".into()),
            ..Default::default()
        };
        store
            .enrich_threads(std::slice::from_mut(&mut thread))
            .await
            .unwrap();
        assert_eq!(thread.project_id, Some(Some("p".into())));
        std::fs::remove_file(roots).unwrap();
        thread.project_id = None;
        store
            .enrich_threads(std::slice::from_mut(&mut thread))
            .await
            .unwrap();
        assert_eq!(thread.project_id, None);
        std::fs::write(&path, "invalid JSON").unwrap();
        assert!(matches!(
            store.load().await,
            Err(DesktopProjectError::Invalid(_))
        ));
        std::fs::remove_file(&path).unwrap();
        assert!(store.load().await.unwrap().projects.is_empty());
        std::fs::write(&path, "{}").unwrap();
        assert!(store.load().await.unwrap().projects.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn inaccessible_project_root_does_not_hide_saved_conversations() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("restricted");
        let root = parent.join("project");
        std::fs::create_dir_all(&root).unwrap();
        let path = directory.path().join("projects.json");
        std::fs::write(&path, serde_json::to_vec(&json!({"local-projects":{"saved":{"id":"saved","name":"Saved","rootPaths":[root]}}})).unwrap()).unwrap();
        let mut thread = Thread {
            id: Some("saved-thread".into()),
            cwd: Some(root.to_str().unwrap().into()),
            ..Default::default()
        };
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = DesktopProjectStore::new(&path)
            .enrich_threads(std::slice::from_mut(&mut thread))
            .await;
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        result.unwrap();
        assert_eq!(thread.project_id, Some(Some("saved".into())));
    }
}
