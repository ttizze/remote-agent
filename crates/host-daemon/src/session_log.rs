//! One writer and one append-only file per session. Only complete, validated
//! records enter the projection. A failed write poisons the writer until reopen.
use crate::platform::FileLock;
use agent_core::session::{Event, Record, Session, project};
use std::{
    io::{BufRead, Seek, Write},
    path::Path,
    sync::{Arc, Mutex},
};

pub(crate) struct SessionLog {
    writer: Arc<Mutex<FileLock>>,
    session: Session,
    seq: u64,
    failed: bool,
}

impl SessionLog {
    pub(crate) fn session(&self) -> &Session {
        &self.session
    }

    /// Publish a complete initial log atomically. Used for creation and legacy
    /// import so a crash cannot expose a half-imported conversation.
    pub(crate) async fn create(path: &Path, events: Vec<(u64, Event)>) -> Result<Self, String> {
        let path = path.to_owned();
        let (file, session, seq) = tokio::task::spawn_blocking(move || {
            let parent = path.parent().ok_or("session log has no parent")?;
            crate::platform::create_state_directory(parent).map_err(|e| e.to_string())?;
            let temporary = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
            let file = temporary.as_file().try_clone().map_err(|e| e.to_string())?;
            file.try_lock().map_err(|e| e.to_string())?;
            let mut writer = FileLock(file);
            let mut session = None;
            let mut seq = 0;
            for (at, event) in events {
                seq += 1;
                let record = Record {
                    version: 1,
                    seq,
                    at,
                    event,
                };
                session = Some(project(session.as_ref(), &record)?);
                serde_json::to_writer(&mut writer.0, &record).map_err(|e| e.to_string())?;
                writer.0.write_all(b"\n").map_err(|e| e.to_string())?;
            }
            let session = session.ok_or("session log is empty")?;
            writer.0.sync_all().map_err(|e| e.to_string())?;
            temporary
                .persist_noclobber(&path)
                .map_err(|e| e.to_string())?;
            #[cfg(unix)]
            std::fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|e| e.to_string())?;
            Ok::<_, String>((writer, session, seq))
        })
        .await
        .map_err(|e| e.to_string())??;
        Ok(Self {
            writer: Arc::new(Mutex::new(file)),
            session,
            seq,
            failed: false,
        })
    }

    pub(crate) async fn open(path: &Path) -> Result<Self, String> {
        let path = path.to_owned();
        let (file, session, seq) = tokio::task::spawn_blocking(move || {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .map_err(|e| e.to_string())?;
            file.try_lock()
                .map_err(|error| format!("cannot lock session log: {error}"))?;
            let mut writer = FileLock(file);
            let file = &mut writer.0;
            let mut reader = std::io::BufReader::new(&mut *file);
            let mut session = None;
            let mut seq = 0;
            let mut valid_end = 0;
            let mut line = Vec::new();
            let mut incomplete = false;
            loop {
                line.clear();
                let count = reader
                    .read_until(b'\n', &mut line)
                    .map_err(|e| e.to_string())?;
                if count == 0 {
                    break;
                }
                if !line.ends_with(b"\n") {
                    incomplete = true;
                    break;
                }
                let record: Record = serde_json::from_slice(&line)
                    .map_err(|e| format!("invalid session log record {}: {e}", seq + 1))?;
                if record.seq != seq + 1 {
                    return Err("session log sequence gap".into());
                }
                session = Some(project(session.as_ref(), &record)?);
                seq = record.seq;
                valid_end += count as u64;
            }
            drop(reader);
            let session = session.ok_or("session log is empty")?;
            if incomplete {
                file.set_len(valid_end)
                    .and_then(|()| file.sync_all())
                    .map_err(|e| e.to_string())?;
            }
            file.seek(std::io::SeekFrom::End(0))
                .map_err(|e| e.to_string())?;
            Ok::<_, String>((writer, session, seq))
        })
        .await
        .map_err(|e| e.to_string())??;
        Ok(Self {
            writer: Arc::new(Mutex::new(file)),
            session,
            seq,
            failed: false,
        })
    }

    /// Streaming chunks are written before publication; execution boundaries,
    /// complete entries and provider checkpoints also flush to durable storage.
    pub(crate) async fn append(&mut self, at: u64, event: Event) -> Result<(), String> {
        if self.failed {
            return Err("session log requires recovery after a failed write".into());
        }
        let durable = !matches!(event, Event::TextAppended { .. });
        let record = Record {
            version: 1,
            seq: self
                .seq
                .checked_add(1)
                .ok_or("session log sequence overflow")?,
            at,
            event,
        };
        let next = project(Some(&self.session), &record)?;
        let mut bytes = serde_json::to_vec(&record).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        // Also poison on cancellation: the bytes may already have reached disk.
        self.failed = true;
        // A canceled append retains the lock in this task until I/O ends.
        let writer = self.writer.clone();
        tokio::task::spawn_blocking(move || {
            let mut writer = writer.lock().map_err(|error| error.to_string())?;
            writer
                .0
                .write_all(&bytes)
                .map_err(|error| error.to_string())?;
            if durable {
                writer.0.sync_data().map_err(|error| error.to_string())?;
            }
            Ok::<_, String>(())
        })
        .await
        .map_err(|error| error.to_string())??;
        self.session = next;
        self.seq = record.seq;
        self.failed = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::session::Target;
    use serde_json::json;

    fn created() -> Vec<(u64, Event)> {
        vec![(
            1,
            Event::Created {
                id: "session".into(),
                cwd: "/workspace".into(),
                title: None,
                selection: Target {
                    provider: "test".into(),
                    model: "model".into(),
                },
            },
        )]
    }

    #[tokio::test]
    async fn append_preserves_existing_bytes_and_replay_recovers_only_incomplete_tail() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("session.jsonl");
        let mut log = SessionLog::create(&path, created()).await.unwrap();
        let prefix = std::fs::read(&path).unwrap();
        log.append(
            2,
            Event::ProviderState {
                provider: "test".into(),
                state: json!({"resume":"opaque"}),
            },
        )
        .await
        .unwrap();
        let expected = log.session().clone();
        assert!(std::fs::read(&path).unwrap().starts_with(&prefix));
        drop(log);
        let length = std::fs::metadata(&path).unwrap().len();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"version\":1,\"seq\":3")
            .unwrap();
        let mut log = SessionLog::open(&path).await.unwrap();
        assert_eq!(log.session(), &expected);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), length);
        log.append(
            3,
            Event::ProviderState {
                provider: "test".into(),
                state: json!({"resume":"new"}),
            },
        )
        .await
        .unwrap();
        let bytes = std::fs::read_to_string(&path).unwrap();
        let records: Vec<Record> = bytes
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            records.iter().map(|record| record.seq).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[tokio::test]
    async fn complete_corruption_gaps_and_unknown_versions_are_not_truncated() {
        for suffix in [
            "broken\n".to_owned(),
            serde_json::to_string(&Record {
                version: 1,
                seq: 3,
                at: 2,
                event: Event::ProviderState {
                    provider: "test".into(),
                    state: json!(null),
                },
            })
            .unwrap()
                + "\n",
            serde_json::to_string(&Record {
                version: 2,
                seq: 2,
                at: 2,
                event: Event::ProviderState {
                    provider: "test".into(),
                    state: json!(null),
                },
            })
            .unwrap()
                + "\n",
        ] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("session.jsonl");
            drop(SessionLog::create(&path, created()).await.unwrap());
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(suffix.as_bytes())
                .unwrap();
            let before = std::fs::read(&path).unwrap();
            assert!(SessionLog::open(&path).await.is_err());
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
    }

    #[tokio::test]
    async fn creation_is_atomic_and_an_open_log_excludes_other_writers() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("session.jsonl");
        let mut events = created();
        events.push((
            2,
            Event::TextAppended {
                execution_id: "missing".into(),
                entry_id: "missing".into(),
                text: "invalid".into(),
            },
        ));
        assert!(SessionLog::create(&path, events).await.is_err());
        assert!(!path.exists());
        let log = SessionLog::create(&path, created()).await.unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(SessionLog::open(&path).await.is_err());
        assert!(SessionLog::create(&path, created()).await.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let inherited = log.writer.lock().unwrap().0.try_clone().unwrap();
        drop(log);
        SessionLog::open(&path)
            .await
            .unwrap_or_else(|error| panic!("reopen: {error}"));
        drop(inherited);
    }

    #[tokio::test]
    async fn failed_write_never_publishes_or_accepts_another_append() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("session.jsonl");
        let mut log = SessionLog::create(&path, created()).await.unwrap();
        let before = log.session().clone();
        {
            let mut writer = log.writer.lock().unwrap();
            writer.0.unlock().unwrap();
            writer.0 = std::fs::File::open(&path).unwrap(); // read-only failure injection
            writer.0.try_lock().unwrap();
        }
        let event = Event::ProviderState {
            provider: "test".into(),
            state: json!("checkpoint"),
        };
        assert!(log.append(2, event.clone()).await.is_err());
        assert_eq!(log.session(), &before);
        assert!(
            log.append(2, event)
                .await
                .unwrap_err()
                .contains("requires recovery")
        );
    }
}
