//! Bounded per-Host outbox. A dedicated worker owns disk IO; producers coalesce snapshots.
use super::ConnectionPerformance;
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, oneshot};

const FILES: usize = 16;
const BYTES: u64 = 256 * 1024;
static DISK: Mutex<()> = Mutex::new(());

enum Command {
    Wake,
    Read(oneshot::Sender<Vec<ConnectionPerformance>>),
    Ack(u64, u64, oneshot::Sender<()>),
}

pub(super) struct Journal {
    pending: Arc<Mutex<Option<ConnectionPerformance>>>,
    commands: mpsc::Sender<Command>,
}

fn sequence(report: &ConnectionPerformance) -> u64 {
    report
        .timeline
        .events
        .last()
        .map_or(0, |event| event.sequence)
}

impl Journal {
    pub fn open(directory: PathBuf, trace: u64) -> io::Result<Self> {
        let pending = Arc::new(Mutex::new(None::<ConnectionPerformance>));
        let snapshots = pending.clone();
        let (commands, mut receiver) = mpsc::channel(8);
        std::thread::Builder::new().name("connection-journal".into()).spawn(move || {
            let mut acknowledged = 0;
            let mut saved = 0;
            while let Some(command) = receiver.blocking_recv() {
                let _disk = DISK.lock().unwrap();
                let pending = snapshots.lock().unwrap().take();
                if let Some(report) = pending && sequence(&report) > acknowledged.max(saved) {
                    if save(&directory, &report).is_ok() {
                        saved = sequence(&report);
                    } else {
                        tracing::warn!(target: "bex", operation = "client.connection.storage", "connection diagnostic could not be saved");
                    }
                }
                match command {
                    Command::Wake => {},
                    Command::Read(reply) => { let _ = reply.send(read(&directory)); },
                    Command::Ack(id, sent, reply) => {
                        if id == trace { acknowledged = acknowledged.max(sent); }
                        let path = directory.join(format!("{id}.json"));
                        if load(&path).is_some_and(|report| sequence(&report) <= sent) {
                            let _ = fs::remove_file(path);
                        }
                        let _ = reply.send(());
                    }
                }
            }
        })?;
        Ok(Self { pending, commands })
    }

    pub fn checkpoint(&self, report: ConnectionPerformance) {
        let mut pending = self.pending.lock().unwrap();
        if pending
            .as_ref()
            .is_none_or(|previous| sequence(previous) < sequence(&report))
        {
            *pending = Some(report);
        }
        drop(pending);
        let _ = self.commands.try_send(Command::Wake);
    }

    pub async fn read(&self) -> Vec<ConnectionPerformance> {
        let (send, receive) = oneshot::channel();
        if self.commands.send(Command::Read(send)).await.is_err() {
            return Vec::new();
        }
        receive.await.unwrap_or_default()
    }

    pub async fn acknowledge(&self, report: &ConnectionPerformance) {
        let (send, receive) = oneshot::channel();
        if self
            .commands
            .send(Command::Ack(report.timeline.id, sequence(report), send))
            .await
            .is_ok()
        {
            let _ = receive.await;
        }
    }
}

fn load(path: &Path) -> Option<ConnectionPerformance> {
    if fs::metadata(path).ok()?.len() > BYTES {
        return None;
    }
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn read(directory: &Path) -> Vec<ConnectionPerformance> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut reports: Vec<_> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|entry| load(&entry.path()))
        .collect();
    reports.sort_by_key(|report| report.timeline.started_at_ms);
    reports
}

fn save(directory: &Path, report: &ConnectionPerformance) -> io::Result<()> {
    use io::Write;
    fs::create_dir_all(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    }
    let bytes = serde_json::to_vec(report)?;
    if bytes.len() as u64 > BYTES {
        return Err(io::Error::other("diagnostic limit exceeded"));
    }
    for entry in fs::read_dir(directory)?.filter_map(Result::ok) {
        if entry.file_name().to_string_lossy().starts_with("pending-") {
            fs::remove_file(entry.path())?;
        }
    }
    let mut file = tempfile::Builder::new()
        .prefix("pending-")
        .tempfile_in(directory)?;
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    file.persist(directory.join(format!("{}.json", report.timeline.id)))?;
    let mut entries: Vec<_> = fs::read_dir(directory)?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .collect();
    entries.sort_by_key(|entry| entry.metadata().and_then(|meta| meta.modified()).ok());
    let excess = entries.len().saturating_sub(FILES);
    for entry in entries.into_iter().take(excess) {
        fs::remove_file(entry.path())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::{ConnectionPhase, ConnectionTimeline, connection::ConnectionEvent};

    fn report(id: u64, sequence: u64) -> ConnectionPerformance {
        ConnectionPerformance {
            recovered: true,
            timeline: ConnectionTimeline {
                id,
                started_at_ms: id,
                events: vec![ConnectionEvent {
                    sequence,
                    at_us: 123,
                    phase: ConnectionPhase::ResumeFailed,
                    group: 7,
                    stream: 0,
                    value: 0,
                }],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn restart_recovers_failed_attempts_and_acknowledges_only_sent_events() {
        let directory = tempfile::tempdir().unwrap();
        let first = Journal::open(directory.path().into(), 1).unwrap();
        first.checkpoint(report(1, 1));
        assert_eq!(first.read().await.len(), 1); // wait for durable checkpoint
        drop(first);
        let second = Journal::open(directory.path().into(), 2).unwrap();
        let pending = second.read().await;
        assert_eq!(
            pending[0].timeline.events[0].phase,
            ConnectionPhase::ResumeFailed
        );
        // A cancelled/failed upload makes no acknowledgment and survives another read.
        assert_eq!(second.read().await.len(), 1);
        second.acknowledge(&pending[0]).await;
        assert!(second.read().await.is_empty());
        second.checkpoint(report(2, 1));
        let sent = second.read().await.remove(0);
        second.checkpoint(report(2, 2));
        second.acknowledge(&sent).await;
        let remaining = second.read().await;
        assert_eq!(sequence(&remaining[0]), 2);
        second.acknowledge(&remaining[0]).await;
        second.checkpoint(report(2, 2)); // teardown must not resurrect delivered events
        assert!(second.read().await.is_empty());
    }

    #[tokio::test]
    async fn storage_is_private_bounded_and_ignores_invalid_files() {
        let directory = tempfile::tempdir().unwrap();
        for id in 1..=20 {
            let journal = Journal::open(directory.path().into(), id).unwrap();
            journal.checkpoint(report(id, 1));
            assert!(journal.read().await.len() <= FILES);
        }
        assert_eq!(read(directory.path()).len(), FILES);
        assert!(directory.path().join("20.json").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(directory.path()).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(directory.path().join("20.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        fs::write(directory.path().join("broken.json"), b"{").unwrap();
        fs::write(
            directory.path().join("oversized.json"),
            vec![b' '; BYTES as usize + 1],
        )
        .unwrap();
        assert_eq!(read(directory.path()).len(), FILES);
    }
}
