//! Host sweeps over the thread list: automatic settlement every minute, and
//! usage-limit recovery and due scheduled tasks on the shared five-second
//! scheduler tick. All derive their work from durable rows, so a restart
//! needs no timers.
use crate::{
    ActorRegistry, Clock, CommandOrigin, ConversationSettings, HostOperations, ScheduledTasks,
    Store, StoreError,
};
use agent_domain::{
    Command, CommandId, ThreadId, ThreadShell, auto_settlement_at, limit_recovery_command,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

pub const SETTLEMENT_INTERVAL: Duration = Duration::from_secs(60);
pub const LIMIT_RECOVERY_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub(crate) struct Sweeps {
    pub store: Store,
    pub registry: Arc<ActorRegistry>,
    pub ops: Arc<dyn HostOperations>,
    pub clock: Arc<dyn Clock>,
    pub scheduled: Arc<ScheduledTasks>,
    /// Settings changed: settle again now.
    pub settings_changed: Arc<Notify>,
}

/// The settle command for one list row under its project's settings.
pub fn settlement_command(
    thread: &ThreadShell,
    after_days: Option<u64>,
    now_ms: i64,
) -> Option<(CommandId, Command)> {
    let settled_at = auto_settlement_at(thread, now_ms, after_days)?;
    let id = format!(
        "server:auto-settle:{}:{}",
        thread.id,
        thread.updated_at.millis()
    );
    Some((
        CommandId::new(id).ok()?,
        Command::SettleAutomatically {
            snapshot_at: thread.updated_at.clone(),
            settled_at: Some(settled_at),
        },
    ))
}

type SweepCommand = (ThreadId, CommandId, Command);

/// Inactivity settlement for every row.
pub fn settlement_commands(
    threads: &[ThreadShell],
    settings: impl Fn(&str) -> ConversationSettings,
    now_ms: i64,
) -> Vec<SweepCommand> {
    threads
        .iter()
        .filter_map(|thread| {
            let after_days = settings(&thread.project).auto_settle_after_days;
            let (id, command) = settlement_command(thread, after_days, now_ms)?;
            Some((thread.id.clone(), id, command))
        })
        .collect()
}

/// Usage-limit recovery sweep.
pub fn limit_recovery_commands(
    threads: &[ThreadShell],
    settings: impl Fn(&str) -> ConversationSettings,
    now_ms: i64,
) -> Vec<SweepCommand> {
    threads
        .iter()
        .filter_map(|thread| {
            let settings = settings(&thread.project);
            let (id, command) = limit_recovery_command(
                thread,
                settings.auto_resume_limited_threads,
                settings.snooze_limited_threads,
                now_ms,
            )?;
            Some((thread.id.clone(), id, command))
        })
        .collect()
}

impl Sweeps {
    async fn rows(&self, limited_only: bool) -> Result<Vec<ThreadShell>, StoreError> {
        self.store
            .blocking(move |store| store.sweep_candidates(limited_only))
            .await
    }

    async fn dispatch(&self, commands: Vec<SweepCommand>) {
        for (thread, id, command) in commands {
            if let Err(error) = self
                .registry
                .dispatch(&thread, id, command, CommandOrigin::Internal)
                .await
            {
                tracing::warn!(%thread, %error, "a sweep command failed");
            }
        }
    }

    pub(crate) async fn settle(&self) -> Result<(), StoreError> {
        let rows = self.rows(false).await?;
        let now = self.clock.now().millis();
        self.dispatch(settlement_commands(&rows, |p| self.ops.settings(p), now))
            .await;
        Ok(())
    }

    pub(crate) async fn recover_limits(&self) -> Result<(), StoreError> {
        let rows = self.rows(true).await?;
        let now = self.clock.now().millis();
        self.dispatch(limit_recovery_commands(
            &rows,
            |p| self.ops.settings(p),
            now,
        ))
        .await;
        Ok(())
    }

    /// The shared five-second tick: limit recovery, then due scheduled tasks.
    async fn tick(&self) -> Result<(), StoreError> {
        self.recover_limits().await?;
        self.scheduled.run_due().await
    }

    /// Runs until aborted.
    pub(crate) async fn run(self) {
        let mut settle = tokio::time::interval(SETTLEMENT_INTERVAL);
        let mut limits = tokio::time::interval(LIMIT_RECOVERY_INTERVAL);
        for interval in [&mut settle, &mut limits] {
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        }
        loop {
            let result = tokio::select! {
                _ = settle.tick() => self.settle().await,
                () = self.settings_changed.notified() => self.settle().await,
                _ = limits.tick() => self.tick().await,
            };
            if let Err(error) = result {
                tracing::warn!(%error, "a thread sweep failed");
            }
        }
    }
}

impl Store {
    /// Live, unarchived list rows; `limited_only` keeps threads stopped by a usage limit.
    pub fn sweep_candidates(&self, limited_only: bool) -> Result<Vec<ThreadShell>, StoreError> {
        self.read(|c| {
            let mut statement = c.prepare_cached(
                "SELECT payload FROM thread_shells
                 WHERE deleted = 0 AND archived = 0
                   AND (?1 = 0 OR json_extract(payload, '$.last_error_class') = 'usage_limit')
                 ORDER BY thread_id",
            )?;
            let rows = statement
                .query_map([limited_only], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            rows.iter()
                .map(|payload| Ok(serde_json::from_str(payload)?))
                .collect()
        })
    }
}

#[cfg(test)]
mod tests;
