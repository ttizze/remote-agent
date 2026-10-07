//! Project setup scripts, typed into the thread's `setup-{id}` terminal. An
//! observed run wraps the command so the shell reports its exit code through
//! a per-run sentinel line.
use crate::terminals::{OpenTerminal, TerminalOutput, Terminals};
use agent_protocol::models::ProjectScript;
use agent_runtime::{SetupEvent, SetupProgress, SetupRequest, StartedSetup};
use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};
use tokio::sync::mpsc;

const OUTPUT_LINE_MAX_LENGTH: usize = 400;
/// A partial line longer than this is a byte stream; only its tail is kept.
const PARTIAL_LINE_MAX_LENGTH: usize = 4_096;
const COMPLETION_SENTINEL_PREFIX: &str = "__SETUP_DONE__";

/// The first script that runs on worktree creation.
pub(crate) fn setup_script(scripts: &[ProjectScript]) -> Option<&ProjectScript> {
    scripts.iter().find(|script| script.run_on_worktree_create)
}

static TERMINAL_CONTROL: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[()][A-Za-z0-9]|\x1b[=>]|[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]",
    )
    .expect("pattern compiles")
});

/// Terminal control sequences removed and the end trimmed.
fn clean(raw: &str) -> String {
    TERMINAL_CONTROL.replace_all(raw, "").trim_end().to_owned()
}

/// At most 400 UTF-16 units.
fn bounded(line: &str) -> String {
    let mut units = 0;
    line.chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= OUTPUT_LINE_MAX_LENGTH
        })
        .collect()
}

/// Which wrapper the setup terminal's shell understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompletionShell {
    Posix,
    Fish,
    PowerShell,
}

/// The shell the terminal starts: PowerShell on Windows, otherwise `$SHELL`.
fn completion_shell(windows: bool, shell: Option<&str>) -> CompletionShell {
    if windows {
        return CompletionShell::PowerShell;
    }
    let shell = shell.unwrap_or_default();
    match shell.rsplit('/').next().unwrap_or(shell) {
        "fish" => CompletionShell::Fish,
        "pwsh" | "powershell" => CompletionShell::PowerShell,
        _ => CompletionShell::Posix,
    }
}

/// The command in a block closed on its own line, so a trailing comment or a
/// heredoc cannot swallow the sentinel, and the shell reads the whole block
/// before running it. `\r` is Enter for every shell's line editor.
fn wrap_command(command: &str, shell: CompletionShell, sentinel: &str) -> String {
    let body = command.replace("\r\n", "\r").replace('\n', "\r");
    match shell {
        CompletionShell::PowerShell => format!(
            "$global:LASTEXITCODE = $null; & {{\r{body}\r}}; if ($null -ne $LASTEXITCODE) {{ $__setupcode = $LASTEXITCODE }} elseif ($?) {{ $__setupcode = 0 }} else {{ $__setupcode = 1 }}; Write-Host \"{sentinel}$__setupcode\""
        ),
        CompletionShell::Fish => format!("begin\r{body}\rend; printf '\\n{sentinel}%s\\n' $status"),
        CompletionShell::Posix => format!("( {body}\r); printf '\\n{sentinel}%s\\n' \"$?\""),
    }
}

/// The exit code after this run's sentinel; `Some(None)` for a malformed one.
fn sentinel_code(line: &str, sentinel: &str) -> Option<Option<i32>> {
    let rest = &line[line.find(sentinel)? + sentinel.len()..];
    let end = rest
        .char_indices()
        .find(|(index, c)| !(c.is_ascii_digit() || (*index == 0 && *c == '-')))
        .map_or(rest.len(), |(index, _)| index);
    let digits = &rest[..end];
    if digits.is_empty() || digits == "-" {
        return None;
    }
    Some(digits.parse().ok())
}

/// Splits terminal text on `\r\n`, `\r` or `\n`; an installer's redrawn
/// progress line becomes a short line of its own.
#[derive(Default)]
struct Lines {
    pending: String,
    bytes: Vec<u8>,
}
impl Lines {
    fn push(&mut self, data: &[u8]) -> Vec<String> {
        self.bytes.extend_from_slice(data);
        let valid = match std::str::from_utf8(&self.bytes) {
            Ok(_) => self.bytes.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(_) => self.bytes.len(),
        };
        let text = String::from_utf8_lossy(&self.bytes[..valid]).into_owned();
        self.bytes.drain(..valid);
        self.pending.push_str(&text);
        let mut lines: Vec<String> = self
            .pending
            .split("\r\n")
            .flat_map(|part| part.split(['\r', '\n']))
            .map(str::to_owned)
            .collect();
        self.pending = lines.pop().unwrap_or_default();
        if self.pending.len() > PARTIAL_LINE_MAX_LENGTH {
            let mut start = self.pending.len() - PARTIAL_LINE_MAX_LENGTH;
            while !self.pending.is_char_boundary(start) {
                start += 1;
            }
            self.pending = self.pending[start..].to_owned();
        }
        lines
    }
}

/// Reads the setup terminal until the sentinel, forwarding cleaned output lines
/// that are not the shell's echo of the wrapper. `None` when the terminal
/// exited or closed first.
async fn observe(
    mut output: mpsc::UnboundedReceiver<TerminalOutput>,
    sentinel: String,
    echoed: Vec<String>,
    progress: SetupProgress,
) -> Option<i32> {
    let mut lines = Lines::default();
    while let Some(event) = output.recv().await {
        let TerminalOutput::Data(data) = event else {
            return None;
        };
        for raw in lines.push(&data) {
            if let Some(code) = sentinel_code(&raw, &sentinel) {
                return code;
            }
            let cleaned = clean(&raw);
            if cleaned.is_empty()
                || cleaned.contains(&sentinel)
                || echoed.iter().any(|line| cleaned.ends_with(line.as_str()))
            {
                continue;
            }
            progress.report(SetupEvent::Output(bounded(&cleaned)));
        }
    }
    None
}

/// The variables a setup script sees; no client can answer color probes yet.
fn setup_environment(project_root: &str, worktree: &str) -> BTreeMap<String, String> {
    [
        ("PROJECT_ROOT", project_root),
        ("WORKTREE_PATH", worktree),
        ("COLORTERM", ""),
        ("NO_COLOR", "1"),
        ("FORCE_COLOR", "0"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect()
}

/// Opens the script's terminal in the workspace and types the command. An
/// observed run's completion closes the shell after a clean exit when nothing
/// else runs there; a failed run keeps it open for a look.
pub(crate) async fn run(
    terminals: &Arc<Terminals>,
    request: &SetupRequest,
    script: &ProjectScript,
) -> Result<StartedSetup, String> {
    let failed = |operation: &str| {
        format!(
            "Project setup script operation '{operation}' failed for thread '{}' in '{}'.",
            request.thread, request.cwd
        )
    };
    let terminal_id = format!("setup-{}", script.id);
    terminals
        .open(OpenTerminal {
            thread: request.thread.clone(),
            terminal_id: terminal_id.clone(),
            cwd: request.cwd.clone(),
            worktree_path: Some(request.cwd.clone()),
            env: setup_environment(&request.project_root, &request.cwd),
            size: None,
        })
        .await
        .map_err(|error| {
            tracing::warn!(operation = "host.setup.open", message = %error);
            failed("openTerminal")
        })?;
    let observed = request.observe.tracked().then(|| {
        let token = uuid::Uuid::new_v4().simple().to_string();
        let sentinel = format!("{COMPLETION_SENTINEL_PREFIX}_{token}:");
        let shell = completion_shell(cfg!(windows), std::env::var("SHELL").ok().as_deref());
        (wrap_command(&script.command, shell, &sentinel), sentinel)
    });
    let command = observed
        .as_ref()
        .map_or_else(|| script.command.clone(), |(wrapped, _)| wrapped.clone());
    // Observe before writing so the sentinel cannot pass unseen.
    let output = match &observed {
        Some(_) => Some(
            terminals
                .observe(&request.thread, &terminal_id)
                .ok_or_else(|| failed("openTerminal"))?,
        ),
        None => None,
    };
    let handle = agent_protocol::operations::thread_terminal_handle_for(
        request.thread.as_str(),
        &terminal_id,
    );
    terminals
        .write(&handle, format!("{command}\r").into_bytes())
        .await
        .map_err(|error| {
            tracing::warn!(operation = "host.setup.write", message = %error);
            failed("writeCommand")
        })?;
    let completion = observed.zip(output).map(|((wrapped, sentinel), output)| {
        let echoed = wrapped
            .split('\r')
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
        let (terminals, thread, terminal) = (
            terminals.clone(),
            request.thread.clone(),
            terminal_id.clone(),
        );
        let progress = request.observe.clone();
        Box::pin(async move {
            let code = observe(output, sentinel, echoed, progress).await;
            if code == Some(0) {
                terminals.close_idle(&thread, Some(&terminal)).await;
            }
            code
        }) as futures_util::future::BoxFuture<'static, Option<i32>>
    });
    Ok(StartedSetup {
        name: script.name.clone(),
        command: script.command.clone(),
        terminal_id,
        run_async: script.run_async != Some(false),
        completion,
    })
}

#[cfg(test)]
mod tests;
