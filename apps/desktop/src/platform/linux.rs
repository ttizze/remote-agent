use std::{
    collections::BTreeMap,
    os::unix::process::CommandExt,
    process::Command,
    sync::{Arc, Condvar, Mutex, OnceLock},
    thread::{self, JoinHandle},
    time::Duration,
};

use super::APPLICATION_IDENTITY;

const UNITY_LAUNCHER_ENTRY_PATH: &str = "/com/canonical/Unity/LauncherEntry";
const UNITY_LAUNCHER_ENTRY_INTERFACE: &str = "com.canonical.Unity.LauncherEntry";
const UNITY_UPDATE_SIGNAL: &str = "Update";

pub(super) fn prepare_host(command: &mut Command) {
    command.process_group(0);
}

/// Linux shells that implement the Unity LauncherEntry protocol consume this
/// signal as the application badge. The mailbox coalesces updates so native
/// UI callers never wait for the session bus, while preserving the latest
/// value (including the clear sent on focus and shutdown).
pub(super) fn set_notification_badge(count: u32) {
    badge_worker().publish(count);
}

/// Flushes and clears the launcher badge before the desktop process exits.
///
/// The worker bounds each bus operation, so joining here cannot wait forever;
/// unlike a regular focus clear this path owns the worker's final zero update.
pub(super) fn shutdown_notification_badge() {
    let Some(worker) = BADGE_WORKER.get() else {
        return;
    };
    worker.shutdown();
}

static BADGE_WORKER: OnceLock<BadgeWorker> = OnceLock::new();

fn badge_worker() -> &'static BadgeWorker {
    BADGE_WORKER.get_or_init(BadgeWorker::start)
}

struct BadgeWorker {
    mailbox: Arc<BadgeMailbox>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl BadgeWorker {
    fn start() -> Self {
        let mailbox = Arc::new(BadgeMailbox::default());
        let worker_mailbox = mailbox.clone();
        let thread = match thread::Builder::new()
            .name("remote-agent-linux-badge".to_owned())
            .spawn(move || run_badge_worker(worker_mailbox))
        {
            Ok(thread) => Some(thread),
            Err(error) => {
                tracing::debug!(
                    target: "desktop",
                    operation = "notification.badge.worker",
                    message = %error,
                );
                None
            }
        };
        Self {
            mailbox,
            thread: Mutex::new(thread),
        }
    }

    fn publish(&self, count: u32) {
        let _ = self.mailbox.publish(count);
    }

    fn shutdown(&self) {
        self.mailbox.stop();
        let thread = self
            .thread
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(thread) = thread {
            if thread.join().is_err() {
                tracing::debug!(
                    target: "desktop",
                    operation = "notification.badge.worker_join",
                    "Linux badge worker stopped unexpectedly",
                );
            }
        }
    }
}

#[derive(Default)]
struct BadgeMailbox {
    state: Mutex<BadgeMailboxState>,
    wake: Condvar,
}

#[derive(Default)]
struct BadgeMailboxState {
    latest: Option<u32>,
    stopping: bool,
}

impl BadgeMailbox {
    fn publish(&self, count: u32) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.stopping {
            return false;
        }
        state.latest = Some(count);
        self.wake.notify_one();
        true
    }

    fn stop(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.stopping = true;
        state.latest = Some(0);
        self.wake.notify_one();
    }

    fn take(&self) -> Option<(u32, bool)> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        loop {
            if let Some(count) = state.latest.take() {
                return Some((count, state.stopping));
            }
            if state.stopping {
                return None;
            }
            state = self
                .wake
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LauncherBadgeUpdate {
    application_uri: String,
    count: i64,
    count_visible: bool,
}

fn launcher_badge_update(count: u32) -> LauncherBadgeUpdate {
    LauncherBadgeUpdate {
        application_uri: format!("application://{APPLICATION_IDENTITY}.desktop"),
        count: i64::from(count),
        count_visible: count != 0,
    }
}

fn run_badge_worker(mailbox: Arc<BadgeMailbox>) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::debug!(
                target: "desktop",
                operation = "notification.badge.runtime",
                message = %error,
            );
            return;
        }
    };

    while let Some((count, stopping)) = mailbox.take() {
        let update = launcher_badge_update(count);
        if let Err(error) = runtime.block_on(publish_badge(&update)) {
            tracing::debug!(
                target: "desktop",
                operation = "notification.badge.publish",
                message = %error,
            );
        }
        if stopping {
            break;
        }
    }
}

const BADGE_BUS_TIMEOUT: Duration = Duration::from_millis(250);

async fn publish_badge(update: &LauncherBadgeUpdate) -> anyhow::Result<()> {
    let connection = tokio::time::timeout(BADGE_BUS_TIMEOUT, zbus::Connection::session()).await??;
    let mut properties = BTreeMap::new();
    properties.insert("count", zbus::zvariant::Value::from(update.count));
    properties.insert(
        "count-visible",
        zbus::zvariant::Value::from(update.count_visible),
    );

    tokio::time::timeout(
        BADGE_BUS_TIMEOUT,
        connection.emit_signal(
            None::<&str>,
            UNITY_LAUNCHER_ENTRY_PATH,
            UNITY_LAUNCHER_ENTRY_INTERFACE,
            UNITY_UPDATE_SIGNAL,
            &(update.application_uri.as_str(), properties),
        ),
    )
    .await??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::launcher_badge_update;

    #[test]
    fn launcher_badge_uses_the_packaged_application_identity() {
        let update = launcher_badge_update(3);

        assert_eq!(
            update.application_uri,
            "application://app.remoteagent.desktop.desktop"
        );
        assert!(
            include_str!("../../linux/app.remoteagent.desktop.desktop")
                .contains("Name=Remote Agent")
        );
        assert_eq!(update.count, 3);
        assert!(update.count_visible);
    }

    #[test]
    fn clearing_badge_hides_the_launcher_count() {
        let update = launcher_badge_update(0);

        assert_eq!(update.count, 0);
        assert!(!update.count_visible);
    }

    #[test]
    fn launcher_counts_are_not_truncated() {
        let update = launcher_badge_update(u32::MAX);

        assert_eq!(update.count, i64::from(u32::MAX));
        assert!(update.count_visible);
    }

    #[test]
    fn mailbox_coalesces_to_the_latest_clear() {
        let mailbox = super::BadgeMailbox::default();

        mailbox.publish(4);
        mailbox.publish(0);

        assert_eq!(mailbox.take(), Some((0, false)));
        mailbox.stop();
        assert_eq!(mailbox.take(), Some((0, true)));
        assert_eq!(mailbox.take(), None);
    }
}
