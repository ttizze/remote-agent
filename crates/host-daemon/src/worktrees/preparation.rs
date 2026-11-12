//! Git preparation reports actual work and observes cancellation while pipes drain.
use agent_protocol::workspace::{SetupStageId, SetupStageStatus};
use anyhow::{Context, Result, anyhow};
use std::{path::Path, process::Stdio, sync::Arc, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_util::sync::CancellationToken;

pub(crate) enum Progress {
    Stage(SetupStageId, SetupStageStatus),
    Base(String),
    Checkout(u32),
    Created { path: String, branch: String },
}

#[derive(Clone)]
pub(crate) struct Observer {
    pub notify: Arc<dyn Fn(Progress) -> Result<()> + Send + Sync>,
    pub cancel: CancellationToken,
}
impl Default for Observer {
    fn default() -> Self {
        Self {
            notify: Arc::new(|_| Ok(())),
            cancel: CancellationToken::new(),
        }
    }
}
impl Observer {
    pub fn send(&self, progress: Progress) -> Result<()> {
        (self.notify)(progress)
    }
}

pub(crate) fn default_base(cwd: &Path) -> Result<String> {
    let remotes = crate::git::text(cwd, &["remote"])?;
    let remote = remotes
        .lines()
        .find(|name| *name == "origin")
        .or_else(|| remotes.lines().next());
    let Some(remote) = remote else {
        return crate::git::text(cwd, &["symbolic-ref", "--short", "HEAD"])
            .map(|name| name.trim().into());
    };
    if let Ok(head) = crate::git::text(
        cwd,
        &[
            "symbolic-ref",
            "--short",
            &format!("refs/remotes/{remote}/HEAD"),
        ],
    ) {
        return Ok(head.trim().into());
    }
    let output = crate::git::bounded(
        cwd,
        &["ls-remote", "--symref", "--", remote, "HEAD"],
        Duration::from_secs(30),
    )?;
    let text = String::from_utf8(output.stdout)?;
    let branch = text
        .lines()
        .find_map(|line| {
            line.strip_prefix("ref: refs/heads/")?
                .strip_suffix("\tHEAD")
        })
        .context("remote default branch is unavailable")?;
    Ok(format!("{remote}/{branch}"))
}

pub(super) fn start_ref(
    cwd: &Path,
    requested: &str,
    fetch: bool,
    observer: &Observer,
) -> Result<String> {
    let base = if requested.is_empty() {
        default_base(cwd)?
    } else {
        requested.into()
    };
    if base.starts_with('-') || base.contains(['\n', '\r', '\0']) {
        return Err(anyhow!("invalid worktree base"));
    }
    observer.send(Progress::Base(base.clone()))?;
    let remote = if fetch {
        remote_target(cwd, &base)?
    } else {
        None
    };
    let Some((remote, branch)) = remote else {
        observer.send(Progress::Stage(
            SetupStageId::Fetch,
            SetupStageStatus::Skipped,
        ))?;
        return Ok(base);
    };
    observer.send(Progress::Stage(
        SetupStageId::Fetch,
        SetupStageStatus::Running,
    ))?;
    let tracking = format!("refs/remotes/{remote}/{branch}");
    let refspec = format!("+refs/heads/{branch}:{tracking}");
    let fetched = match git(
        cwd,
        &["fetch", "--no-tags", "--", &remote, &refspec],
        observer,
    ) {
        Ok(()) => true,
        Err(error)
            if error.to_string().lines().any(|line| {
                line == format!("fatal: couldn't find remote ref refs/heads/{branch}")
            }) =>
        {
            false
        }
        Err(error) => return Err(error),
    };
    observer.send(Progress::Stage(SetupStageId::Fetch, SetupStageStatus::Done))?;
    if !fetched {
        return Ok(base);
    }
    crate::git::text(
        cwd,
        &["rev-parse", "--verify", &format!("{tracking}^{{commit}}")],
    )?;
    // Keep the fetched reference as the worktree's comparison base, so later
    // upstream updates and the native "vs origin/main" summary use that branch.
    Ok(tracking)
}

fn remote_target(cwd: &Path, base: &str) -> Result<Option<(String, String)>> {
    let text = crate::git::text(cwd, &["remote"])?;
    let mut remotes: Vec<_> = text.lines().collect();
    remotes.sort_by_key(|name| std::cmp::Reverse(name.len()));
    let branch = base.strip_prefix("refs/heads/").unwrap_or(base);
    let local_ref = format!("refs/heads/{branch}");
    let upstream = crate::git::text(
        cwd,
        &[
            "for-each-ref",
            "--format=%(refname)\t%(upstream:remotename)\t%(upstream:remoteref)",
            &local_ref,
        ],
    )?;
    let local = upstream
        .lines()
        .map(|line| line.split('\t').collect::<Vec<_>>())
        .find(|fields| fields.first() == Some(&local_ref.as_str()));
    if let Some(fields) = &local
        && fields.len() == 3
        && remotes.contains(&fields[1])
        && let Some(branch) = fields[2].strip_prefix("refs/heads/")
    {
        return Ok(Some((fields[1].into(), branch.into())));
    }
    let name = base.strip_prefix("refs/remotes/").unwrap_or(base);
    if local.is_none()
        && let Some(target) = remotes.iter().find_map(|remote| {
            name.strip_prefix(&format!("{remote}/"))
                .map(|branch| ((*remote).to_owned(), branch.to_owned()))
        })
    {
        return Ok(Some(target));
    }
    Ok(remotes
        .contains(&"origin")
        .then(|| ("origin".into(), branch.into())))
}

pub(super) fn git(cwd: &Path, args: &[&str], observer: &Observer) -> Result<()> {
    tokio::runtime::Handle::current().block_on(observed_git(cwd, args, observer))
}

async fn observed_git(cwd: &Path, args: &[&str], observer: &Observer) -> Result<()> {
    let mut child = agent_process::command(Path::new("git"))?
        .arg("--no-optional-locks")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .spawn()
        .context("failed to run git")?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    // The supervisor owns Git's process group/Job Object. Closing this pipe
    // cancels SSH, checkout filters and every other descendant before it exits.
    let lifetime = child.stdin.take().unwrap();
    let progress = observer.clone();
    let out = tokio::spawn(drain(stdout, None));
    let err = tokio::spawn(async move {
        let result = drain(stderr, Some(progress.clone())).await;
        if result.is_err() {
            progress.cancel.cancel();
        }
        result
    });
    let result = tokio::select! {
        result = child.wait() => result.map_err(Into::into),
        _ = observer.cancel.cancelled() => Err(anyhow!("workspace preparation cancelled")),
        _ = tokio::time::sleep(Duration::from_secs(300)) => Err(anyhow!("git preparation timed out")),
    };
    drop(lifetime);
    if result.is_err() {
        let _ = child.wait().await;
    }
    out.await.context("git output reader failed")??;
    let stderr = err.await.context("git output reader failed")??;
    if result?.success() {
        Ok(())
    } else {
        Err(anyhow!(String::from_utf8_lossy(&stderr).trim().to_owned()))
    }
}

async fn drain(mut pipe: impl AsyncRead + Unpin, observer: Option<Observer>) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut line = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let count = pipe.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        for part in buffer[..count].split_inclusive(|byte| matches!(byte, b'\r' | b'\n')) {
            line.extend(part.iter().take(64 * 1024 - line.len().min(64 * 1024)));
            if matches!(part.last(), Some(b'\r' | b'\n')) {
                if let Some(observer) = &observer {
                    let text = String::from_utf8_lossy(&line);
                    if let Some((_, after)) = text.split_once("Updating files:")
                        && let Some((percent, _)) = after.trim().split_once('%')
                        && let Ok(percent) = percent.parse::<u32>()
                    {
                        observer.send(Progress::Checkout(percent.min(100)))?;
                    }
                }
                output.extend(
                    line.drain(..)
                        .take(1024 * 1024 - output.len().min(1024 * 1024)),
                );
            }
        }
    }
    output.extend(
        line.into_iter()
            .take(1024 * 1024 - output.len().min(1024 * 1024)),
    );
    Ok(output)
}
