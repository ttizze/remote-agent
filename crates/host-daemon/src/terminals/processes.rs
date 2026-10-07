//! Which command a terminal's shell runs, from one process table snapshot
//! shared by every terminal.
use futures_util::future::BoxFuture;
use std::{collections::HashMap, sync::Arc, time::Duration};

const MAX_POLL_INTERVAL: Duration = Duration::from_secs(60);
const PS_TIMEOUT: Duration = Duration::from_secs(1);
const PS_MAX_OUTPUT_BYTES: usize = 524_288;

pub(crate) type ProcessSource =
    Arc<dyn Fn() -> BoxFuture<'static, Result<ProcessTable, String>> + Send + Sync>;

/// What a shell runs: nothing, or a command and its name when known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Subprocess {
    Idle,
    Running(Option<String>),
}
impl Subprocess {
    pub(crate) fn running(&self) -> bool {
        matches!(self, Self::Running(_))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProcessTable {
    children: HashMap<u32, Vec<u32>>,
    commands: HashMap<u32, String>,
}

impl ProcessTable {
    /// `ps -eo pid=,ppid=,comm=` output; the command may contain spaces.
    pub(crate) fn parse(stdout: &str) -> Self {
        let mut table = Self::default();
        for line in stdout.lines() {
            let mut fields = line.trim_start().splitn(2, char::is_whitespace);
            let (Some(pid), Some(rest)) = (fields.next(), fields.next()) else {
                continue;
            };
            let mut rest = rest.trim_start().splitn(2, char::is_whitespace);
            let (Some(ppid), Some(command)) = (rest.next(), rest.next()) else {
                continue;
            };
            let (Ok(pid), Ok(ppid)) = (pid.parse::<u32>(), ppid.parse::<u32>()) else {
                continue;
            };
            let command = command.trim();
            if command.is_empty() {
                continue;
            }
            table.insert(pid, ppid, command);
        }
        table
    }

    pub(crate) fn insert(&mut self, pid: u32, ppid: u32, command: &str) {
        self.commands.insert(pid, command.trim().to_owned());
        self.children.entry(ppid).or_default().push(pid);
    }

    fn command_name(&self, pid: u32) -> Option<String> {
        command_name(self.commands.get(&pid).map_or("", String::as_str))
    }

    /// The first child of the shell that is not a childless copy of the shell
    /// itself, which async prompt themes fork while they wait.
    pub(crate) fn subprocess(&self, shell: u32) -> Subprocess {
        let shell_name = self.command_name(shell);
        let child = self.children.get(&shell).and_then(|children| {
            children.iter().copied().find(|pid| {
                shell_name.is_none()
                    || self.command_name(*pid) != shell_name
                    || self.children.get(pid).is_some_and(|c| !c.is_empty())
            })
        });
        match child {
            None => Subprocess::Idle,
            Some(pid) => Subprocess::Running(
                self.command_name(pid)
                    .map(|name| name.chars().take(super::MAX_LABEL_LENGTH).collect()),
            ),
        }
    }
}

/// The executable's base name: brackets and arguments dropped.
pub(crate) fn command_name(raw: &str) -> Option<String> {
    let mut trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if (trimmed.starts_with('[') && trimmed.ends_with(']'))
        || (trimmed.starts_with('(') && trimmed.ends_with(')'))
    {
        trimmed = trimmed[1..trimmed.len() - 1].trim();
    }
    let first = trimmed.split_whitespace().next()?;
    let separators: &[char] = if cfg!(windows) { &['\\', '/'] } else { &['/'] };
    let base = first.rsplit(separators).next().unwrap_or(first);
    let base = match base.len().checked_sub(4) {
        Some(end) if cfg!(windows) && base[end..].eq_ignore_ascii_case(".exe") => &base[..end],
        _ => base,
    };
    (!base.is_empty()).then(|| base.to_owned())
}

/// The next poll after `failures` failed snapshots in a row.
pub(crate) fn poll_delay(interval: Duration, failures: u32) -> Duration {
    interval
        .saturating_mul(2u32.saturating_pow(failures))
        .min(MAX_POLL_INTERVAL)
}

/// `ps` on Unix; a partial or failed listing is not a snapshot.
pub(super) fn system() -> ProcessSource {
    let program = ["/bin/ps", "/usr/bin/ps"]
        .into_iter()
        .find(|path| std::path::Path::new(path).is_file())
        .unwrap_or("ps");
    Arc::new(move || {
        Box::pin(async move {
            if cfg!(windows) {
                return Err("process snapshots are unavailable on this platform".into());
            }
            let mut command = tokio::process::Command::new(program);
            command
                .args(["-eo", "pid=,ppid=,comm="])
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true);
            let output = tokio::time::timeout(PS_TIMEOUT, command.output())
                .await
                .map_err(|_| "ps timed out".to_owned())?
                .map_err(|error| error.to_string())?;
            if !output.status.success() || output.stdout.len() > PS_MAX_OUTPUT_BYTES {
                return Err(format!("ps failed: {}", output.status));
            }
            Ok(ProcessTable::parse(&String::from_utf8_lossy(
                &output.stdout,
            )))
        })
    })
}
