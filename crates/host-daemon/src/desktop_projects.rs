use std::{
    env, io,
    path::{Path, PathBuf},
};

use serde_json::Value;

mod state;

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

    async fn load(&self) -> Result<state::Snapshot, DesktopProjectError> {
        let metadata = match tokio::fs::metadata(&self.path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(state::Snapshot::default());
            }
            Err(error) => return Err(DesktopProjectError::Read(error)),
        };
        if metadata.len() > MAX_STATE_BYTES {
            return Err(DesktopProjectError::TooLarge(metadata.len()));
        }
        let bytes = tokio::fs::read(&self.path)
            .await
            .map_err(DesktopProjectError::Read)?;
        state::Snapshot::parse(&bytes).map_err(map_state_error)
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
    use super::*;

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
}
