//! Atomic file replacement and owner-only state files.
use anyhow::{Context, Result};
use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
};

/// Stable and Nightly use the existing shared directory. Source development
/// uses its dev child, independently of the installed release channel.
pub fn state_directory(development: bool) -> Result<PathBuf> {
    let directory = directories::ProjectDirs::from("app", "bex", "BEX")
        .context("application data directory unavailable")?
        .data_local_dir()
        .to_owned();
    Ok(if development {
        directory.join("dev")
    } else {
        directory
    })
}

pub(crate) fn open_lock(path: &Path) -> Result<File> {
    File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(Into::into)
}

/// Unix state is owner-only; Windows directories inherit the parent DACL.
/// The default state parent is the current user's local application data folder.
pub fn create_state_directory(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn private_file_options() -> fs::OpenOptions {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// Atomically replace owner-only JSON state and flush its contents before rename.
pub fn save_private_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    save_private(path, |file| {
        serde_json::to_writer(file, value).map_err(Into::into)
    })
}

/// Atomically replace an opaque payload with the same privacy and durability as JSON state.
pub fn save_private_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    save_private(path, |file| std::io::Write::write_all(file, bytes))
}

fn save_private(path: &Path, write: impl Fn(&mut File) -> io::Result<()>) -> Result<()> {
    let parent = path.parent().context("state path has no parent")?;
    create_state_directory(parent)?;
    #[cfg(windows)]
    let _writer = {
        // Windows cannot replace a destination while a competing replacement
        // is still deleting its old file. Coordinate publishers across both
        // threads and processes; readers still see an atomic complete file.
        let mut lock_path = path.as_os_str().to_owned();
        lock_path.push(".lock");
        let file = open_lock(Path::new(&lock_path))?;
        file.lock()?;
        file
    };
    write_atomic(path, write)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("could not save state {}", path.display()))
}

/// Flush and atomically replace a file, tolerating transient Windows reader locks.
/// The callback may run again after a failed OS operation; its own errors are final.
/// New files start owner-only. A caller replacing a workspace file must preserve
/// its original permissions in the callback.
pub fn write_atomic<E>(
    path: &Path,
    write: impl Fn(&mut File) -> std::result::Result<(), E>,
) -> std::result::Result<(), atomicwrites::Error<E>> {
    let mut retries = 0;
    loop {
        let result = atomicwrites::AtomicFile::new(path, atomicwrites::AllowOverwrite)
            .write_with_options(&write, private_file_options());
        // A reader outside the caller's writer lock can temporarily hold the Windows
        // destination without delete sharing. Retry only OS failures, never
        // serialization errors. Every attempt atomically replaces the complete,
        // flushed document.
        if cfg!(windows)
            && let Err(atomicwrites::Error::Internal(error)) = &result
            && matches!(error.raw_os_error(), Some(5 | 32 | 33))
            && retries < 10
        {
            retries += 1;
            std::thread::sleep(std::time::Duration::from_millis(10 * retries));
            continue;
        }
        return result;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn failed_write_preserves_the_original_and_can_be_retried_by_its_owner() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.bin");
        fs::write(&path, "original").unwrap();
        let calls = std::cell::Cell::new(0);
        let result = write_atomic(&path, |file| {
            calls.set(calls.get() + 1);
            file.write_all(b"partial")?;
            Err::<(), _>(io::Error::from_raw_os_error(5))
        });
        assert!(matches!(result, Err(atomicwrites::Error::User(_))));
        assert_eq!(calls.get(), 1, "callback errors must never be retried");
        assert_eq!(fs::read(&path).unwrap(), b"original");
        write_atomic(&path, |file| file.write_all(b"complete")).unwrap();
        assert_eq!(fs::read(path).unwrap(), b"complete");
    }

    #[cfg(windows)]
    #[test]
    fn replacement_recovers_when_a_windows_reader_releases_its_handle() {
        use std::os::windows::fs::OpenOptionsExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.bin");
        fs::write(&path, "original").unwrap();
        // A reader may allow other reads and writes while denying replacement.
        let reader = File::options()
            .read(true)
            .share_mode(3)
            .open(&path)
            .unwrap();
        let (attempted, attempts) = std::sync::mpsc::channel();
        let worker = {
            let path = path.clone();
            std::thread::spawn(move || {
                write_atomic(&path, |file| {
                    attempted.send(()).unwrap();
                    file.write_all(b"replacement")
                })
            })
        };
        attempts
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        // Keep the reader through the first failed replacement. Always release
        // it before joining, including when an implementation gives up early.
        let retried = attempts.recv_timeout(std::time::Duration::from_secs(2));
        assert_eq!(fs::read(&path).unwrap(), b"original");
        drop(reader);
        let saved = worker.join().unwrap();
        assert!(retried.is_ok(), "the temporary reader lock was not retried");
        saved.unwrap();
        assert_eq!(fs::read(path).unwrap(), b"replacement");
    }

    #[test]
    fn source_development_has_its_own_state_beneath_the_shared_release_directory() {
        let shared = state_directory(false).unwrap();
        let development = state_directory(true).unwrap();
        assert_eq!(development, shared.join("dev"));
    }

    #[test]
    fn opaque_state_preserves_every_byte() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.bin");
        let bytes: Vec<u8> = (0..=255).collect();
        save_private_bytes(&path, &bytes).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn concurrent_state_replacements_keep_complete_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        save_private_json(&path, &serde_json::json!({"writer": 0, "sequence": 0})).unwrap();
        let start = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for writer in 0..8 {
                let (path, start) = (&path, &start);
                scope.spawn(move || {
                    start.wait();
                    for sequence in 0..50 {
                        save_private_json(
                            path,
                            &serde_json::json!({"writer": writer, "sequence": sequence}),
                        )
                        .unwrap();
                        let saved: serde_json::Value =
                            serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
                        assert!(saved["writer"].as_u64().unwrap() < 8);
                        assert!(saved["sequence"].as_u64().unwrap() < 50);
                    }
                });
            }
        });
    }

    #[cfg(windows)]
    #[test]
    fn state_save_waits_for_a_temporary_windows_file_lock() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        save_private_json(&path, &"previous").unwrap();
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .unwrap();
        std::thread::scope(|scope| {
            scope.spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(60));
                drop(held);
            });
            save_private_json(&path, &"updated").unwrap();
        });
        assert_eq!(fs::read_to_string(path).unwrap(), "\"updated\"");
    }

    #[test]
    fn serialization_failure_preserves_previous_state_and_names_the_path() {
        struct Invalid;
        impl serde::Serialize for Invalid {
            fn serialize<S: serde::Serializer>(
                &self,
                _: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("invalid state"))
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        save_private_json(&path, &"previous").unwrap();
        let error = save_private_json(&path, &Invalid).unwrap_err();
        assert!(format!("{error:#}").contains("invalid state"));
        assert!(error.to_string().contains("state.json"));
        assert_eq!(fs::read_to_string(path).unwrap(), "\"previous\"");
    }
}
