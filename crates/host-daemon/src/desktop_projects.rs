use std::{
    env, io,
    path::{Path, PathBuf},
};

use serde_json::Value;
use tokio::io::AsyncReadExt;

pub(crate) mod state;
pub(crate) mod titles;

pub const HOST_PROJECT_LIST_METHOD: &str = "host/project/list";
pub const HOST_THREAD_LIST_METHOD: &str = "host/thread/list";
pub const HOST_THREAD_READ_METHOD: &str = "host/thread/read";
pub const HOST_THREAD_START_METHOD: &str = "host/thread/start";
pub const HOST_PROJECT_METHODS: &[&str] = &[
    HOST_PROJECT_LIST_METHOD,
    HOST_THREAD_LIST_METHOD,
    HOST_THREAD_READ_METHOD,
    HOST_THREAD_START_METHOD,
];

const MAX_STATE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct DesktopProjectStore {
    path: PathBuf,
}

impl DesktopProjectStore {
    pub fn from_environment() -> Result<Self, DesktopProjectError> {
        let codex_home = env::var_os("CODEX_HOME").map(PathBuf::from).or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".codex"))
        });
        let codex_home = codex_home.ok_or(DesktopProjectError::MissingHome)?;
        Ok(Self::new(codex_home.join(".codex-global-state.json")))
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub async fn project_list(&self, params: &Value) -> Result<Value, DesktopProjectError> {
        let snapshot = self.load().await?;
        snapshot.project_list(params).map_err(map_state_error)
    }

    pub async fn enrich_threads(&self, result: Value) -> Result<Value, DesktopProjectError> {
        let snapshot = self.load().await?;
        Ok(snapshot.enrich_threads(result))
    }

    pub(crate) async fn load(&self) -> Result<state::Snapshot, DesktopProjectError> {
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
        let mut snapshot = state::Snapshot::parse(&bytes).map_err(map_state_error)?;
        snapshot.worktree_roots = crate::worktrees::workspace_roots(&self.path).await
            .map_err(|error| DesktopProjectError::Read(io::Error::other(error)))?;
        Ok(snapshot)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn map_state_error(error: state::Error) -> DesktopProjectError {
    match error {
        state::Error::Invalid(error) => DesktopProjectError::Invalid(error),
        state::Error::InvalidCursor => DesktopProjectError::InvalidCursor,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DesktopProjectError {
    #[error("neither CODEX_HOME nor HOME is available")]
    MissingHome,
    #[error("failed to read Codex Desktop project state: {0}")]
    Read(#[source] io::Error),
    #[error("Codex Desktop project state is too large ({0} bytes)")]
    TooLarge(u64),
    #[error("invalid Codex Desktop project state: {0}")]
    Invalid(#[source] serde_json::Error),
    #[error("invalid project list cursor")]
    InvalidCursor,
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

    #[test]
    fn maps_state_errors_to_public_errors() {
        assert!(matches!(
            map_state_error(state::Error::InvalidCursor),
            DesktopProjectError::InvalidCursor
        ));

        let error = serde_json::from_str::<Value>("{").unwrap_err();
        assert!(matches!(
            map_state_error(state::Error::Invalid(error)),
            DesktopProjectError::Invalid(_)
        ));
    }

    #[tokio::test]
    async fn missing_state_file_returns_an_empty_snapshot() {
        let path = temporary_path("missing");
        let snapshot = DesktopProjectStore::new(path).load().await.unwrap();

        assert_eq!(
            snapshot.project_list(&Value::Null).unwrap(),
            json!({
                "data": [],
                "nextCursor": null,
            })
        );
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
}
