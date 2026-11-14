#[path = "../pty.rs"]
mod pty;
// No history or provider protocol parsing: only owned process lifetimes.
use process_wrap::tokio::{CommandWrap, KillOnDrop};
use std::process::Stdio;
use tokio::process::Command;

fn main() {
    #[cfg(unix)]
    agent_process::reset_signal_mask().expect("reset supervisor signal mask");
    // BROWSER accepts an executable, without arguments, on Windows as well as
    // Unix. Reuse this companion as a successful no-op browser so no host-local
    // callback tab can escape a provider's explicitly client-owned login flow.
    if suppressed_browser_request(
        std::env::var_os("AGENT_SUPPRESS_BROWSER").as_deref() == Some(std::ffi::OsStr::new("1")),
        &std::env::args_os().skip(1).collect::<Vec<_>>(),
    ) {
        return;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("create supervisor runtime");
    let code = runtime
        .block_on(async {
            if std::env::args().nth(1).as_deref() == Some("--agent-pty") {
                pty::run().await
            } else {
                run().await
            }
        })
        .unwrap_or_else(|error| {
            eprintln!("provider supervisor failed: {error}");
            1
        });
    // Tokio's stdin reader may still be blocked after a native process exits.
    // All owned children have already been killed/reaped before reaching here.
    std::process::exit(code);
}

async fn run() -> std::io::Result<i32> {
    #[cfg(unix)]
    provider_file_limit()?;
    let mut args = std::env::args_os().skip(1);
    let program = args
        .next()
        .ok_or_else(|| std::io::Error::other("provider program is required"))?;
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut command = CommandWrap::from(command);
    #[cfg(unix)]
    command.wrap(process_wrap::tokio::ProcessGroup::leader());
    #[cfg(windows)]
    command.wrap(process_wrap::tokio::JobObject);
    #[cfg(not(any(unix, windows)))]
    return Err(std::io::Error::other(
        "provider process ownership is unsupported on this OS",
    ));
    let mut child = command.wrap(KillOnDrop).spawn()?;
    let mut input = child
        .stdin()
        .take()
        .ok_or_else(|| std::io::Error::other("provider stdin unavailable"))?;
    let mut source = tokio::io::stdin();
    let finished = tokio::select! {
        result = child.wait() => Some(result),
        _ = tokio::io::copy(&mut source, &mut input) => None,
    };
    drop(input);
    let result = match finished {
        Some(result) => result.map(|status| status.code().unwrap_or(1)),
        None => {
            match tokio::time::timeout(std::time::Duration::from_millis(250), child.wait()).await {
                Ok(result) => result.map(|status| status.code().unwrap_or(1)),
                Err(_) => Ok(0),
            }
        }
    };
    // Also terminate remaining tool descendants after the main CLI exits.
    let _ = std::pin::Pin::from(child.kill()).await;
    result
}

#[cfg(unix)]
fn provider_file_limit() -> std::io::Result<()> {
    // macOS GUI launches inherit a soft limit of 256. One shared Codex server
    // owns the MCP pipes, history files and tool processes for several chats,
    // so it can exhaust that budget before spawning a command. Like Node's
    // initialization (also used by T3 Code), raise the soft limit within the
    // inherited hard limit, searching up to 2^20 when it is unlimited.
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut lower = limit.rlim_cur;
    let mut upper = if limit.rlim_max == libc::RLIM_INFINITY {
        1 << 20
    } else {
        limit.rlim_max
    };
    while lower < upper {
        let candidate = lower + (upper - lower).div_ceil(2);
        limit.rlim_cur = candidate;
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) } == 0 {
            lower = candidate;
        } else {
            upper = candidate - 1;
        }
    }
    Ok(())
}

fn suppressed_browser_request(enabled: bool, args: &[std::ffi::OsString]) -> bool {
    enabled
        && args.len() == 1
        && args[0]
            .to_str()
            .is_some_and(|url| url.starts_with("https://") || url.starts_with("http://"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suppresses_only_explicit_browser_invocations_and_keeps_provider_commands() {
        let url = ["https://claude.com/oauth/authorize?state=test".into()];
        assert!(suppressed_browser_request(true, &url));
        assert!(!suppressed_browser_request(false, &url));
        assert!(!suppressed_browser_request(true, &[]));
        assert!(!suppressed_browser_request(true, &["claude.exe".into()]));
        assert!(!suppressed_browser_request(
            true,
            &["claude.exe".into(), "auth".into(), "login".into()]
        ));
        assert!(!suppressed_browser_request(
            true,
            &[url[0].clone(), "extra".into()]
        ));
        assert!(!suppressed_browser_request(
            true,
            &["file:///tmp/test".into()]
        ));
    }
}
