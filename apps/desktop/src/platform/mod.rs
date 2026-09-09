use agent_core::{
    state::Snapshot,
    store::Store,
    transport::{Endpoint, Identity, Relays, Ticket},
};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "linux")]
use linux as os;
#[cfg(target_os = "macos")]
use macos as os;
#[cfg(target_os = "windows")]
use windows as os;

#[cfg(any(target_os = "linux", target_os = "windows"))]
mod microphone;
pub(crate) use os::{Recording, start_recording};
pub(crate) enum RecordingEvent {
    Started,
    Finished(Result<Vec<u8>, String>),
}

pub(crate) fn state_dir() -> Result<PathBuf, String> {
    std::env::var_os("BEX_STATE_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            directories::ProjectDirs::from("app", "bex", "BEX")
                .map(|project| project.data_local_dir().to_path_buf())
        })
        .ok_or_else(|| "application data directory unavailable".into())
}

async fn local_identity(directory: &Path) -> Result<Identity, String> {
    let directory = directory.to_owned();
    tokio::task::spawn_blocking(move || {
        let directory = directory
            .canonicalize()
            .map_err(|error| error.to_string())?;
        match std::env::var("BEX_KEY_STORAGE").as_deref() {
            Ok("file") => host_daemon::load_local_identity(&host_daemon::FileKeyStore(
                directory.join("identity.keys"),
            )),
            Err(_) | Ok("keyring") => {
                host_daemon::load_local_identity(&host_daemon::KeyringStore::new(
                    directory.to_str().ok_or("state directory is not UTF-8")?,
                )?)
            }
            Ok(_) => Err("BEX_KEY_STORAGE must be keyring or file".into()),
        }
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Each view owns its endpoint and session through Store. The local identity is
/// provisioned only by the daemon, using the same secure-record implementation.
pub(crate) async fn connect(remote: Option<&str>, snapshot: Snapshot) -> Result<Store, String> {
    let directory = state_dir()?;
    if let Some(remote) = remote {
        let identity = local_identity(&directory).await?;
        let ticket: Ticket = remote
            .parse()
            .map_err(|error: agent_core::transport::TransportError| error.to_string())?;
        let endpoint = Endpoint::bind(identity, Relays::Default)
            .await
            .map_err(|error| error.to_string())?;
        return Store::connect(&endpoint, &ticket, snapshot, None)
            .await
            .map_err(|error| error.to_string());
    }

    let ticket_path = directory.join("host.ticket");
    // The daemon's file lock distinguishes a running instance from a stale
    // public ticket. Starting a view never re-provisions credentials.
    host_daemon::platform::create_state_directory(&directory).map_err(|error| error.to_string())?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("host.lock"))
        .map_err(|error| error.to_string())?;
    let start = match lock.try_lock() {
        Ok(()) => {
            lock.unlock().map_err(|error| error.to_string())?;
            true
        }
        Err(std::fs::TryLockError::WouldBlock) => false,
        Err(std::fs::TryLockError::Error(error)) => return Err(error.to_string()),
    };
    let previous_ticket = std::fs::metadata(&ticket_path)
        .and_then(|metadata| metadata.modified())
        .ok();
    let mut child = if start {
        Some(start_host(&directory)?)
    } else {
        None
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let ready = loop {
        let fresh = !start
            || std::fs::metadata(&ticket_path)
                .and_then(|metadata| metadata.modified())
                .ok()
                != previous_ticket;
        if fresh
            && let Ok(ticket) = std::fs::read_to_string(&ticket_path)
            && let Ok(ticket) = ticket.trim().parse::<Ticket>()
        {
            // A previous ticket is usable only once the daemon owns its lock.
            match lock.try_lock() {
                Err(std::fs::TryLockError::WouldBlock) => break Ok(ticket),
                Ok(()) => lock.unlock().map_err(|error| error.to_string())?,
                Err(std::fs::TryLockError::Error(error)) => break Err(error.to_string()),
            }
        }
        if let Some(child) = child.as_mut()
            && let Some(status) = child.try_wait().map_err(|error| error.to_string())?
            && lock.try_lock().is_ok()
        {
            lock.unlock().map_err(|error| error.to_string())?;
            break Err(format!("Host exited during startup ({status})"));
        }
        if tokio::time::Instant::now() >= deadline {
            break Err("Host startup timed out".into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    if let Some(mut child) = child {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
    let ticket = ready?;
    let identity = local_identity(&directory).await?;
    let endpoint = Endpoint::bind(identity, Relays::Disabled)
        .await
        .map_err(|error| error.to_string())?;
    Store::connect(&endpoint, &ticket, snapshot, None)
        .await
        .map_err(|error| error.to_string())
}

fn start_host(directory: &Path) -> Result<std::process::Child, String> {
    let executable = std::env::var_os("BEX_HOST_DAEMON")
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(|| {
            std::env::current_exe().map(|path| {
                path.with_file_name(format!("host-daemon{}", std::env::consts::EXE_SUFFIX))
            })
        })
        .map_err(|error| error.to_string())?;
    let mut command = Command::new(executable);
    command
        .arg("--state-dir")
        .arg(directory)
        .arg("--key-storage")
        .arg(std::env::var_os("BEX_KEY_STORAGE").unwrap_or_else(|| "keyring".into()))
        .arg("--codex")
        .arg(std::env::var_os("BEX_CODEX").unwrap_or_else(|| "codex".into()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    os::prepare_host(&mut command);
    command.spawn().map_err(|error| error.to_string())
}

pub(crate) fn choose_files() -> Option<Vec<PathBuf>> {
    rfd::FileDialog::new().pick_files()
}
pub(crate) fn choose_folder() -> Option<PathBuf> {
    rfd::FileDialog::new().pick_folder()
}
pub(crate) fn choose_destination(name: &str) -> Option<PathBuf> {
    rfd::FileDialog::new().set_file_name(name).save_file()
}

pub(crate) fn save_snapshot(path: &Path, snapshot: &impl Serialize) -> Result<(), String> {
    let parent = path.parent().ok_or("snapshot path has no parent")?;
    host_daemon::platform::create_state_directory(parent).map_err(|error| error.to_string())?;
    atomicwrites::AtomicFile::new(path, atomicwrites::AllowOverwrite)
        .write_with_options(
            |file| {
                serde_json::to_writer(&mut *file, snapshot)?;
                file.sync_all()
            },
            os::private_file_options(),
        )
        .map_err(|error| error.to_string())
}
