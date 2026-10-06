use super::{ExecutorContext, retry};
use crate::{Durability, EffectError, EffectHandler, EffectJob, Store, StoreError};
use agent_domain::{
    CapturedBaseline, CheckpointFile, CheckpointId, CheckpointScope, CheckpointScopeId,
    CheckpointStatus, Effect, EffectBody, EffectResult, Input, Run, RunAttemptId, State, ThreadId,
};
use base64::Engine as _;
use futures_util::future::BoxFuture;
use rusqlite::{OptionalExtension, params};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

type Heads = BTreeMap<String, Option<String>>;

fn sha256_hex(value: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, value.as_bytes())
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The hidden Git ref of a scope's checkpoint (T3 `checkpointRefForScopeOrdinal`).
pub fn checkpoint_reference(scope: &CheckpointScopeId, ordinal: u64) -> String {
    let key = &sha256_hex(scope.as_str())[..32];
    format!(
        "refs/t3/orchestration-v2/checkpoints/{}/ordinal/{ordinal}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key)
    )
}

pub fn checkpoint_id(scope: &CheckpointScopeId, ordinal: u64) -> CheckpointId {
    CheckpointId::new(format!("checkpoint:{scope}:{ordinal}")).expect("derived checkpoint id")
}

/// One scope per thread and directory, so refs of different checkouts of one
/// repository never collide.
pub fn checkpoint_scope(thread: &ThreadId, cwd: &str) -> CheckpointScope {
    CheckpointScope {
        id: CheckpointScopeId::new(format!("scope:{thread}:{}", &sha256_hex(cwd)[..16]))
            .expect("derived scope id"),
        cwd: cwd.to_owned(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Baseline {
    pub checkpoint: CheckpointId,
    pub file_ref: String,
    pub native_heads: Heads,
}

impl Store {
    pub fn checkpoint_baseline(
        &self,
        scope: &CheckpointScopeId,
        ordinal: u64,
    ) -> Result<Option<Baseline>, StoreError> {
        self.read(|c| {
            c.query_row(
                "SELECT checkpoint_id, file_ref, native_heads FROM checkpoint_baselines
                 WHERE scope_id = ?1 AND ordinal = ?2",
                params![scope.as_str(), ordinal as i64],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
            .map(|(checkpoint, file_ref, heads)| {
                Ok(Baseline {
                    checkpoint: CheckpointId::new(checkpoint)
                        .map_err(|error| StoreError::Corrupt(error.to_string()))?,
                    file_ref,
                    native_heads: serde_json::from_str(&heads)?,
                })
            })
            .transpose()
        })
    }

    async fn record_baseline(
        &self,
        scope: &CheckpointScopeId,
        ordinal: u64,
        heads: &Heads,
    ) -> Result<(), StoreError> {
        let (scope, heads) = (scope.clone(), serde_json::to_string(heads)?);
        self.write(move |tx| {
            tx.execute(
                "INSERT OR IGNORE INTO checkpoint_baselines
                     (scope_id, ordinal, checkpoint_id, file_ref, native_heads)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    scope.as_str(),
                    ordinal as i64,
                    checkpoint_id(&scope, ordinal).as_str(),
                    checkpoint_reference(&scope, ordinal),
                    heads,
                ],
            )?;
            Ok(())
        })
        .await
    }
}

/// Captures the workspace before a provider turn into the scope's baseline
/// `max(0, ordinal - 1)` unless it exists (T3 `captureBaseline`), then starts the turn.
pub(crate) struct BaselineBeforeStart {
    pub(crate) context: ExecutorContext,
    pub(crate) inner: Arc<dyn EffectHandler>,
}

impl EffectHandler for BaselineBeforeStart {
    fn durability(&self) -> Durability {
        self.inner.durability()
    }
    fn should_run(&self, state: &State, effect: &Effect) -> bool {
        self.inner.should_run(state, effect)
    }
    fn run(&self, job: EffectJob) -> BoxFuture<'_, Result<Option<EffectResult>, EffectError>> {
        Box::pin(async move {
            if let Some(attempt) = &job.effect.attempt {
                let state = self.context.state(&job.thread).await?;
                if let Some(run) = state
                    .runs
                    .iter()
                    .find(|run| run.attempt.as_ref() == Some(attempt))
                    && let Err(error) = self.baseline(&job.thread, &state, run, attempt).await
                {
                    tracing::warn!(run = %run.id, %error,
                        "checkpoint baseline capture failed; starting the provider without a baseline");
                }
            }
            self.inner.run(job).await
        })
    }
    fn failure(&self, effect: &Effect, error: &str) -> Option<EffectResult> {
        self.inner.failure(effect, error)
    }
}

impl BaselineBeforeStart {
    /// A run that started outside workspace preparation (an imported, forked or
    /// delegated thread) first gets the scope of the thread's directory.
    async fn baseline(
        &self,
        thread: &ThreadId,
        state: &State,
        run: &Run,
        attempt: &RunAttemptId,
    ) -> Result<(), String> {
        let scope = match &run.checkpoint_scope {
            Some(scope) => scope.clone(),
            None => {
                let Some(cwd) = self.context.cwd(state) else {
                    return Ok(());
                };
                let scope = checkpoint_scope(thread, &cwd);
                self.context
                    .input(
                        thread,
                        Input::CheckpointScope {
                            run: Some(run.id.clone()),
                            attempt: Some(attempt.clone()),
                            scope: Some(scope.clone()),
                        },
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                scope
            }
        };
        let ordinal = run.ordinal.saturating_sub(1);
        // A rolled-back checkpoint's ref may outlive the rollback that made it stale.
        let stale = state.checkpoints.iter().any(|checkpoint| {
            checkpoint.scope.as_ref() == Some(&scope)
                && checkpoint.run_ordinal == ordinal
                && checkpoint.status == CheckpointStatus::Stale
        });
        capture_baseline(
            &self.context,
            &scope,
            ordinal,
            &run.native_baseline_heads,
            stale,
        )
        .await
    }
}

async fn capture_baseline(
    context: &ExecutorContext,
    scope: &CheckpointScope,
    ordinal: u64,
    heads: &Heads,
    stale: bool,
) -> Result<(), String> {
    let ops = &context.ops;
    if !ops.is_git_repository(scope.cwd.clone()).await {
        return Ok(());
    }
    let reference = checkpoint_reference(&scope.id, ordinal);
    if stale {
        ops.delete_checkpoints(scope.cwd.clone(), vec![reference.clone()])
            .await?;
    }
    if !ops
        .has_checkpoint(scope.cwd.clone(), reference.clone())
        .await?
    {
        ops.capture_checkpoint(scope.cwd.clone(), reference).await?;
    }
    context
        .store
        .record_baseline(&scope.id, ordinal, heads)
        .await
        .map_err(|error| error.to_string())
}

/// Captures a finished run's workspace and the scope baselines it still lacks.
/// Git failures become a `missing` or `error` checkpoint; the run still finishes.
pub(crate) struct CaptureCheckpoint(pub(crate) ExecutorContext);

impl EffectHandler for CaptureCheckpoint {
    fn durability(&self) -> Durability {
        Durability::ReplaySafe
    }
    fn run(&self, job: EffectJob) -> BoxFuture<'_, Result<Option<EffectResult>, EffectError>> {
        Box::pin(async move {
            let EffectBody::CaptureCheckpoint {
                scope,
                native_baseline_heads,
                run,
            } = &job.effect.body
            else {
                return Err(EffectError::Permanent("not a checkpoint capture".into()));
            };
            let state = self.0.state(&job.thread).await?;
            // A rollback can land while a failed capture waits to retry; the
            // workspace then holds the rollback target and the run stays discarded.
            let Some(target) = state.runs.iter().find(|candidate| {
                &candidate.id == run
                    && candidate.attempt == job.effect.attempt
                    && candidate.checkpoint.is_none()
                    && state.captures.contains_key(run)
            }) else {
                return Ok(None);
            };
            let ordinal = target.ordinal;
            let mut baselines = Vec::new();
            for baseline in BTreeSet::from([0, ordinal.saturating_sub(1)]) {
                // A missing, failed or stale baseline is materialized again (T3
                // CheckpointCaptureService skips only ready ones).
                let ready = state.checkpoints.iter().any(|checkpoint| {
                    checkpoint.scope.as_ref() == Some(scope)
                        && checkpoint.run_ordinal == baseline
                        && checkpoint.status == CheckpointStatus::Ready
                });
                if baseline >= ordinal || ready {
                    continue;
                }
                let heads = if baseline + 1 == ordinal {
                    native_baseline_heads.clone()
                } else {
                    Heads::new()
                };
                baselines.push(
                    self.materialize(scope, baseline, heads, job.will_retry)
                        .await?,
                );
            }
            let (status, files) = capture(&self.0, scope, ordinal).await;
            self.0.ops.run_finalized(&job.thread, run, &scope.cwd);
            Ok(Some(EffectResult::CheckpointCaptured {
                status,
                baselines,
                run: run.clone(),
                attempt: job.effect.attempt.clone(),
                checkpoint: checkpoint_id(&scope.id, ordinal),
                file_ref: checkpoint_reference(&scope.id, ordinal),
                files,
            }))
        })
    }
}

impl CaptureCheckpoint {
    /// T3 `materializeBaselineCheckpoint`: ready when its ref exists.
    async fn materialize(
        &self,
        scope: &CheckpointScope,
        ordinal: u64,
        heads: Heads,
        will_retry: bool,
    ) -> Result<CapturedBaseline, EffectError> {
        let (lookup, id) = (self.0.store.clone(), scope.id.clone());
        // The last attempt settles the run with the heads it has rather than
        // leaving it waiting for a capture that will never run again.
        let recorded = match lookup
            .blocking(move |store| store.checkpoint_baseline(&id, ordinal))
            .await
        {
            Ok(recorded) => recorded,
            Err(error) if will_retry => return Err(retry(error)),
            Err(error) => {
                tracing::warn!(scope = %scope.id, ordinal, %error, "recorded baseline heads are unavailable");
                None
            }
        };
        let reference = checkpoint_reference(&scope.id, ordinal);
        let ops = &self.0.ops;
        let available = ops.is_git_repository(scope.cwd.clone()).await
            && match ops
                .has_checkpoint(scope.cwd.clone(), reference.clone())
                .await
            {
                Ok(exists) => exists,
                Err(error) => {
                    tracing::warn!(scope = %scope.id, %reference, %error, "baseline ref lookup failed");
                    false
                }
            };
        Ok(CapturedBaseline {
            status: if available {
                CheckpointStatus::Ready
            } else {
                CheckpointStatus::Missing
            },
            checkpoint: checkpoint_id(&scope.id, ordinal),
            ordinal,
            file_ref: reference,
            native_heads: recorded.map_or(heads, |recorded| recorded.native_heads),
        })
    }
}

/// T3 `CheckpointService.capture`: the summary diffs against the previous
/// ordinal's ref (itself for ordinal 0) and is empty when that ref or the diff
/// is unavailable.
async fn capture(
    context: &ExecutorContext,
    scope: &CheckpointScope,
    ordinal: u64,
) -> (CheckpointStatus, Vec<CheckpointFile>) {
    let ops = &context.ops;
    if !ops.is_git_repository(scope.cwd.clone()).await {
        return (CheckpointStatus::Missing, vec![]);
    }
    let reference = checkpoint_reference(&scope.id, ordinal);
    if let Err(error) = ops
        .capture_checkpoint(scope.cwd.clone(), reference.clone())
        .await
    {
        tracing::warn!(scope = %scope.id, %reference, %error, "checkpoint capture failed");
        return (CheckpointStatus::Error, vec![]);
    }
    let previous = checkpoint_reference(&scope.id, ordinal.saturating_sub(1));
    let previous_exists = match ops
        .has_checkpoint(scope.cwd.clone(), previous.clone())
        .await
    {
        Ok(exists) => exists,
        Err(error) => {
            tracing::warn!(scope = %scope.id, reference = %previous, %error, "previous checkpoint ref lookup failed");
            false
        }
    };
    if !previous_exists {
        return (CheckpointStatus::Ready, vec![]);
    }
    let files = match ops
        .checkpoint_files(scope.cwd.clone(), previous, reference.clone())
        .await
    {
        Ok(files) => files,
        Err(error) => {
            tracing::warn!(scope = %scope.id, %reference, %error, "checkpoint diff summary failed");
            vec![]
        }
    };
    (CheckpointStatus::Ready, files)
}

pub const SHARED_WORKSPACE_RESTORE_MESSAGE: &str = "File restore requires an isolated worktree. This workspace may contain changes from another thread. Rewind the conversation without restoring files instead.";

/// A checkpoint snapshots the whole checkout, so files are restored only in the
/// thread's own worktree that no other live thread's workspace, checkpoint scope
/// or provider session overlaps (T3 `isCheckpointRestoreIsolated`).
pub(crate) async fn restore_isolated(
    context: &ExecutorContext,
    thread: &ThreadId,
    state: &State,
    scope_cwd: &str,
) -> Result<bool, String> {
    let ops = &context.ops;
    let real = |path: &str| {
        let path = path.to_owned();
        async move {
            ops.real_path(path.clone())
                .await
                .map_err(|error| format!("{path}: {error}"))
        }
    };
    let Some(worktree) = state
        .thread
        .as_ref()
        .and_then(|thread| thread.workspace.as_ref())
        .and_then(|workspace| workspace.worktree_path.clone())
    else {
        return Ok(false);
    };
    let (Some(cwd), Some(worktree)) = (real(scope_cwd).await?, real(&worktree).await?) else {
        return Err("the checkpoint workspace no longer exists".into());
    };
    if cwd != worktree {
        return Ok(false);
    }
    let others = context
        .store
        .blocking(|store| store.live_threads())
        .await
        .map_err(|error| error.to_string())?;
    let mut checked = BTreeSet::new();
    for other in others.into_iter().filter(|other| &other.id != thread) {
        let id = other.id.clone();
        let mut paths = context
            .store
            .blocking(move |store| store.checkpoint_scope_cwds(&id))
            .await
            .map_err(|error| error.to_string())?;
        paths.extend(context.sessions.session_cwds(&other.id));
        match other
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.worktree_path.clone())
        {
            Some(worktree) => paths.push(worktree),
            // T3 includes deleted projects; the Host never unregisters one.
            None => match ops.project(&other.project) {
                Some(project) => paths.push(project.root),
                None => return Ok(false),
            },
        }
        for candidate in paths {
            if !checked.insert(candidate.clone()) {
                continue;
            }
            if let Some(other_cwd) = real(&candidate).await?
                && (contains(&cwd, &other_cwd) || contains(&other_cwd, &cwd))
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn contains(parent: &str, child: &str) -> bool {
    Path::new(child).starts_with(Path::new(parent))
}
