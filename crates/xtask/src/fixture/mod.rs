//! Deterministic Codex subprocess. Its configuration belongs to one fixture
//! directory, never the user's Codex home or the parent process environment.

mod history;
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
    pub trace: bool,
    pub expected_cwd: Option<PathBuf>,
    pub stream_delay_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
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
        let program = directory.join("bex-codex-fixture");
        // A symlink would be canonicalized by the real Codex launcher and lose
        // this directory. A hard link keeps isolation without copying a binary.
        match fs::hard_link(executable, &program) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
                fs::copy(executable, &program)?;
            }
            Err(error) => return Err(error.into()),
        }
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
    if arguments.first().map(String::as_str) == Some("app-server")
        && arguments.get(1).map(String::as_str) == Some("generate-json-schema")
    {
        let output = arguments
            .windows(2)
            .find(|pair| pair[0] == "--out")
            .map(|pair| Path::new(&pair[1]))
            .ok_or("--out is required")?;
        fs::create_dir_all(output)?;
        let methods = [
            "initialize",
            "model/list",
            "thread/list",
            "thread/start",
            "thread/read",
            "thread/turns/list",
            "thread/items/list",
            "thread/resume",
            "turn/start",
            "turn/steer",
            "turn/interrupt",
        ];
        let entries: Vec<_> = methods
            .iter()
            .map(|method| serde_json::json!({"properties":{"method":{"enum":[method]}}}))
            .collect();
        fs::write(
            output.join("ClientRequest.json"),
            serde_json::to_vec(&serde_json::json!({"oneOf":entries}))?,
        )?;
        return Ok(());
    }
    if arguments.first().map(String::as_str) != Some("app-server") {
        return Err("usage: bex-codex-fixture app-server [generate-json-schema --out PATH]".into());
    }
    let program = std::env::current_exe()?;
    let home = program
        .parent()
        .ok_or("fixture executable has no directory")?
        .to_path_buf();
    let config = serde_json::from_slice(&fs::read(home.join("fixture-config.json"))?)?;
    server::run(home, config).await
}
