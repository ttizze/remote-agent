use std::{
    collections::BTreeMap,
    os::unix::process::CommandExt,
    process::Command,
    sync::{Arc, Condvar, Mutex, OnceLock},
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
    let mailbox = BADGE_MAILBOX
        .get_or_init(|| {
            let mailbox = Arc::new(BadgeMailbox::default());
            let worker_mailbox = mailbox.clone();
            if let Err(error) = std::thread::Builder::new()
                .name("remote-agent-linux-badge".to_owned())
                .spawn(move || run_badge_worker(worker_mailbox))
            {
                tracing::debug!(
                    target: "desktop",
                    operation = "notification.badge.worker",
                    message = %error,
                );
            }
            mailbox
        })
        .clone();
    mailbox.publish(count);
}

static BADGE_MAILBOX: OnceLock<Arc<BadgeMailbox>> = OnceLock::new();

#[derive(Default)]
struct BadgeMailbox {
    latest: Mutex<Option<u32>>,
    wake: Condvar,
}

impl BadgeMailbox {
    fn publish(&self, count: u32) {
        let mut latest = self.latest.lock().unwrap_or_else(|error| error.into_inner());
        *latest = Some(count);
        self.wake.notify_one();
    }

    fn take(&self) -> u32 {
        let mut latest = self.latest.lock().unwrap_or_else(|error| error.into_inner());
        loop {
            if let Some(count) = latest.take() {
                return count;
            }
            latest = self
                .wake
                .wait(latest)
                .unwrap_or_else(|error| error.into_inner());
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LauncherBadgeUpdate {
    application_identity: &'static str,
    application_uri: String,
    count: i64,
    count_visible: bool,
}

fn launcher_badge_update(count: u32) -> LauncherBadgeUpdate {
    LauncherBadgeUpdate {
        application_identity: APPLICATION_IDENTITY,
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

    loop {
        let update = launcher_badge_update(mailbox.take());
        if let Err(error) = runtime.block_on(publish_badge(&update)) {
            tracing::debug!(
                target: "desktop",
                operation = "notification.badge.publish",
                identity = update.application_identity,
                message = %error,
            );
        }
    }
}

async fn publish_badge(update: &LauncherBadgeUpdate) -> zbus::Result<()> {
    let connection = zbus::Connection::session().await?;
    let mut properties = BTreeMap::new();
    properties.insert("count", zbus::zvariant::Value::from(update.count));
    properties.insert(
        "count-visible",
        zbus::zvariant::Value::from(update.count_visible),
    );

    connection
        .emit_signal(
            None::<&str>,
            UNITY_LAUNCHER_ENTRY_PATH,
            UNITY_LAUNCHER_ENTRY_INTERFACE,
            UNITY_UPDATE_SIGNAL,
            &(update.application_uri, properties),
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::{launcher_badge_update, APPLICATION_IDENTITY};

    #[test]
    fn launcher_badge_uses_the_installed_application_identity() {
        let update = launcher_badge_update(3);

        assert_eq!(update.application_identity, APPLICATION_IDENTITY);
        assert_eq!(update.application_uri, "application://app.remoteagent.desktop.desktop");
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
    fn badge_counts_are_not_truncated() {
        let update = launcher_badge_update(u32::MAX);

        assert_eq!(update.count, i64::from(u32::MAX));
        assert!(update.count_visible);
    }

    #[test]
    fn mailbox_coalesces_to_the_latest_clear() {
        let mailbox = super::BadgeMailbox::default();

        mailbox.publish(4);
        mailbox.publish(0);

        assert_eq!(mailbox.take(), 0);
    }
}
