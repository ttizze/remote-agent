//! Deterministic Codex subprocess. Its configuration belongs to one fixture
//! directory, never the user's Codex home or the parent process environment.

mod accounts;
pub(crate) mod history;
mod scenario;
mod server;

use crate::Result;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Serialize, Deserialize)]
pub struct Config {
    /// Native elapsed-time UI checks need an epoch timestamp near the wall clock.
    #[serde(default)]
    pub live_clock: bool,
    #[serde(default)]
    pub deferred_thread_metadata: bool,
    #[serde(default)]
    pub initialize_gate: Option<PathBuf>,
    pub trace: bool,
    pub expected_cwd: Option<PathBuf>,
    pub stream_delay_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            live_clock: false,
            deferred_thread_metadata: false,
            initialize_gate: None,
            trace: false,
            expected_cwd: None,
            stream_delay_ms: 2000,
        }
    }
}

impl Config {
    pub fn install(&self, executable: &Path, directory: &Path) -> Result<PathBuf> {
        fs::create_dir_all(directory)?;
        let directory = directory.canonicalize()?;
        let program = directory.join(format!("bex-codex-fixture{}", std::env::consts::EXE_SUFFIX));
        // The launcher resolves symlinks, so use a private executable copy.
        // A hard link would share an inode with mutable Cargo outputs while
        // native UI and Rust tests build different feature combinations.
        fs::copy(executable, &program)?;
        fs::write(
            directory.join("fixture-config.json"),
            serde_json::to_vec(self)?,
        )?;
        Ok(program)
    }

    fn delay(&self) -> Duration {
        Duration::from_millis(self.stream_delay_ms)
    }
}

pub async fn run(arguments: &[String]) -> Result<()> {
    let mut arguments = arguments;
    let mut credential_home = None;
    while arguments.first().map(String::as_str) == Some("-c") {
        if arguments.get(1).map(String::as_str) != Some("cli_auth_credentials_store=\"keyring\"") {
            return Err("unsupported fixture configuration override".into());
        }
        credential_home = Some(PathBuf::from(
            std::env::var_os("CODEX_HOME").ok_or("credential helper requires CODEX_HOME")?,
        ));
        arguments = &arguments[2..];
    }
    if arguments != ["app-server"] && arguments != ["app-server", "--listen", "stdio://"] {
        return Err("usage: bex-codex-fixture app-server --listen stdio://".into());
    }
    let program = std::env::current_exe()?;
    let home = program
        .parent()
        .ok_or("fixture executable has no directory")?
        .to_path_buf();
    let config = serde_json::from_slice(&fs::read(home.join("fixture-config.json"))?)?;
    // Only the explicit auth-helper override uses the supplied credential home.
    // Ordinary fixtures never read an inherited personal CODEX_HOME.
    server::run(credential_home.unwrap_or(home), config).await
}
