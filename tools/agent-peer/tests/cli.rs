use anyhow::{Context, Result};
use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use serde_json::{Value, json};
use std::{
    env, fs,
    io::{Read, Write},
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

const SESSION: &str = "12345678-1234-4234-8234-123456789abc";
const OTHER: &str = "87654321-4321-4321-8321-cba987654321";

// The integration-test executable doubles as a deterministic provider subprocess.
// This exercises real pipes, signals and descendants without an interpreter or AI service.
fn fixture(mode: &str) -> Result<()> {
    if mode == "descendant" {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        return runtime.block_on(async {
            let _term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            fs::write("descendant.pid", std::process::id().to_string())?;
            std::future::pending::<Result<()>>().await
        });
    }
    if mode == "stall" {
        let mut child = Command::new(env::current_exe()?)
            .env("AGENT_PEER_FIXTURE", "descendant")
            .spawn()?;
        wait_for(|| Path::new("descendant.pid").exists());
        fs::write("ready", "")?;
        child.wait()?;
        return Ok(());
    }
    let args: Vec<_> = env::args().skip(1).collect();
    fs::write("arguments.json", serde_json::to_vec(&args)?)?;
    if mode == "large" {
        println!("{}", " ".repeat(256 * 1024));
        std::io::stdout().flush()?;
    }
    let mut prompt = String::new();
    std::io::stdin().read_to_string(&mut prompt)?;
    let session = args
        .windows(2)
        .find(|a| a[0] == "--resume" || a[0] == "resume")
        .map(|a| a[1].as_str())
        .unwrap_or(SESSION);
    let text = fs::read_to_string(session).unwrap_or(prompt.clone());
    fs::write(session, prompt)?;
    let id = if mode == "wrong-id" {
        OTHER
    } else if mode == "invalid-id" {
        "invalid"
    } else {
        session
    };
    if mode == "malformed" {
        println!("not JSON");
        return Ok(());
    }
    if args.first().is_some_and(|a| a == "-p") {
        let mut reply = json!({"type":"result", "session_id":id, "result":text});
        if mode == "denied" {
            reply["permission_denials"] = json!([{"tool_name":"Read"}]);
        }
        if mode == "missing-id" {
            reply.as_object_mut().unwrap().remove("session_id");
        }
        println!("{reply}");
    } else {
        if mode != "missing-id" {
            println!("{}", json!({"type":"thread.started","thread_id":id}));
        }
        println!(
            "{}",
            json!({"type":"item.completed","item":{"type":"agent_message","text":text}})
        );
        if mode == "failed" {
            println!(
                "{}",
                json!({"type":"turn.failed","error":{"message":"quota exhausted"}})
            );
        } else if mode != "incomplete" {
            println!("{}", json!({"type":"turn.completed"}));
        }
    }
    if mode == "exit-error" {
        std::process::exit(1);
    }
    Ok(())
}

fn peer(root: &Path, provider: &str, mode: &str, session: Option<&str>) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agent-peer"));
    command
        .env(
            format!("AGENT_PEER_{}_BIN", provider.to_uppercase()),
            env::current_exe().unwrap(),
        )
        .env("AGENT_PEER_FIXTURE", mode);
    command.args([if session.is_some() { "reply" } else { "ask" }, provider]);
    command.args(session).arg("--cwd").arg(root);
    command
}

fn answer(command: &mut Command, prompt: &str) -> (Output, Value) {
    let mut child = command
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(prompt.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{error}: {output:?}"));
    (output, value)
}

fn protocols() {
    for provider in ["claude", "codex"] {
        let root = TempDir::new().unwrap();
        let prompt = "Literal $(touch UNEXPECTED) `touch UNEXPECTED`\n日本語";
        let (output, first) = answer(
            peer(root.path(), provider, "echo", None).args(["--model", "test-model"]),
            prompt,
        );
        assert!(output.status.success(), "{first}");
        assert_eq!(first["status"], "ok");
        assert_eq!(first["text"], prompt);
        assert_eq!(first["provider"], provider);
        assert_eq!(
            first["cwd"],
            root.path().canonicalize().unwrap().to_str().unwrap()
        );
        let args: Vec<String> =
            serde_json::from_slice(&fs::read(root.path().join("arguments.json")).unwrap()).unwrap();
        assert!(args.windows(2).any(|a| a == ["--model", "test-model"]));
        if provider == "claude" {
            for flag in ["--tools", "--allowedTools"] {
                assert!(args.windows(2).any(|a| a == [flag, "Read,Glob,Grep"]));
            }
            assert!(args.iter().any(|a| a == "--strict-mcp-config"));
            assert!(args.iter().any(|a| a == r#"{"disableAllHooks":true}"#));
        } else {
            assert!(args.windows(2).any(|a| a == ["--sandbox", "read-only"]));
            assert_eq!(args.last().unwrap(), "-");
        }
        let mut reply = peer(root.path(), provider, "echo", Some(SESSION));
        let (output, resumed) = answer(&mut reply, "recall");
        assert!(output.status.success(), "{resumed}");
        assert_eq!(resumed["text"], prompt);
        assert_eq!(resumed["session_id"], SESSION);
        assert!(!root.path().join("UNEXPECTED").exists());
    }
    let root = TempDir::new().unwrap();
    let prompt = "入力\n".repeat(100_000);
    let (output, result) = answer(&mut peer(root.path(), "codex", "large", None), &prompt);
    assert!(output.status.success(), "{result}");
    assert_eq!(result["text"], prompt);
}

fn failures() {
    for (provider, mode, expected) in [
        ("claude", "denied", "blocked"),
        ("codex", "incomplete", "error"),
        ("codex", "failed", "error"),
        ("claude", "malformed", "error"),
        ("codex", "malformed", "error"),
        ("claude", "invalid-id", "error"),
        ("codex", "invalid-id", "error"),
        ("claude", "missing-id", "error"),
        ("codex", "missing-id", "error"),
        ("claude", "exit-error", "error"),
        ("codex", "exit-error", "error"),
    ] {
        let root = TempDir::new().unwrap();
        let (output, result) = answer(
            &mut peer(root.path(), provider, mode, None),
            "Partial answer",
        );
        assert!(!output.status.success(), "{provider}/{mode}: {result}");
        assert_eq!(result["status"], expected);
        if mode == "denied" {
            assert_eq!(result["denied_tools"], json!(["Read"]));
        }
        if mode == "failed" {
            assert_eq!(result["errors"], json!(["quota exhausted"]));
            assert_eq!(result["text"], "Partial answer");
        }
    }
    for mode in ["wrong-id", "missing-id"] {
        let root = TempDir::new().unwrap();
        let mut command = peer(root.path(), "codex", mode, Some(SESSION));
        let (output, result) = answer(&mut command, "resume");
        assert_eq!(output.status.success(), mode == "missing-id", "{result}");
        assert_eq!(result["session_id"], SESSION);
    }
    for timeout in ["0", "NaN", "inf"] {
        let root = TempDir::new().unwrap();
        let (output, result) = answer(
            peer(root.path(), "codex", "echo", None).args(["--timeout", timeout]),
            "prompt",
        );
        assert!(!output.status.success(), "{result}");
        assert!(!root.path().join("arguments.json").exists());
    }
}

fn paths_and_validation() {
    let root = TempDir::new().unwrap();
    let home = root.path();
    let project = home.join("project");
    let bin = home.join(".local/bin");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&bin).unwrap();
    fs::copy(env::current_exe().unwrap(), bin.join("claude")).unwrap();
    for path in [bin.as_os_str(), std::ffi::OsStr::new("/nonexistent")] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_agent-peer"));
        command
            .env_remove("AGENT_PEER_CLAUDE_BIN")
            .env("AGENT_PEER_FIXTURE", "echo")
            .env("HOME", home)
            .env("PATH", path)
            .args(["ask", "claude", "--cwd", "~/project"]);
        let (output, result) = answer(&mut command, "literal prompt");
        assert!(output.status.success(), "{result}");
        assert_eq!(
            result["cwd"],
            project.canonicalize().unwrap().to_str().unwrap()
        );
    }
    for prompt in ["", " \n\t"] {
        let (output, result) = answer(&mut peer(home, "codex", "echo", None), prompt);
        assert!(!output.status.success(), "{result}");
        assert_eq!(result["errors"], json!(["Prompt must not be empty"]));
    }
    let (output, result) = answer(
        &mut peer(&home.join("missing"), "codex", "echo", None),
        "prompt",
    );
    assert!(!output.status.success(), "{result}");
    assert_eq!(
        result["errors"],
        json!(["--cwd must be an existing directory"])
    );
}

fn wait_for(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !check() {
        assert!(Instant::now() < deadline, "Timed out waiting for fixture");
        thread::sleep(Duration::from_millis(10));
    }
}

fn cancellation() {
    for signal in [None, Some(Signal::SIGINT), Some(Signal::SIGTERM)] {
        let root = TempDir::new().unwrap();
        let mut command = peer(root.path(), "codex", "stall", None);
        command.args([
            "--timeout",
            if signal.is_none() { "1" } else { "30" },
            "prompt",
        ]);
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        wait_for(|| root.path().join("ready").exists());
        let descendant: i32 = fs::read_to_string(root.path().join("descendant.pid"))
            .unwrap()
            .parse()
            .unwrap();
        if let Some(signal) = signal {
            kill(Pid::from_raw(child.id() as i32), signal).unwrap();
        }
        wait_for(|| child.try_wait().unwrap().is_some());
        let output = child.wait_with_output().unwrap();
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(!output.status.success(), "{result}");
        assert!(
            result["errors"][0]
                .as_str()
                .unwrap()
                .contains(if signal.is_none() {
                    "Timed out"
                } else {
                    "Interrupted"
                })
        );
        wait_for(|| {
            kill(Pid::from_raw(descendant), None).is_err()
                || fs::read_to_string(format!("/proc/{descendant}/stat"))
                    .is_ok_and(|s| s.split_once(") ").is_some_and(|(_, s)| s.starts_with('Z')))
        });
    }
}

fn package(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    let skill = root.join("share/agent-peer");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(skill.join("scripts")).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_agent-peer"), bin.join("agent-peer")).unwrap();
    fs::copy(
        env!("CARGO_BIN_EXE_agent-peer-install-skills"),
        bin.join("agent-peer-install-skills"),
    )
    .unwrap();
    fs::write(skill.join("SKILL.md"), include_str!("../SKILL.md")).unwrap();
    symlink("../../../bin/agent-peer", skill.join("scripts/agent-peer")).unwrap();
    bin.join("agent-peer-install-skills")
}

fn installation() {
    let root = TempDir::new().unwrap();
    let old = root.path().join("old");
    let new = root.path().join("new");
    let old_installer = package(&old);
    let new_installer = package(&new);
    let homes = root.path().join("homes");
    let codex = homes.join("codex");
    let claude = homes.join("claude");
    let install = |binary: &Path| {
        Command::new(binary)
            .env("PATH", "/nonexistent")
            .env("CODEX_HOME", &codex)
            .env("CLAUDE_CONFIG_DIR", &claude)
            .output()
            .unwrap()
    };
    for binary in [&old_installer, &new_installer, &new_installer] {
        let output = install(binary);
        assert!(output.status.success(), "{output:?}");
        for home in [&codex, &claude] {
            let linked = home.join("skills/agent-peer");
            assert_eq!(
                linked.canonicalize().unwrap(),
                binary
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("share/agent-peer")
                    .canonicalize()
                    .unwrap()
            );
            assert!(
                Command::new(linked.join("scripts/agent-peer"))
                    .env("PATH", "/nonexistent")
                    .arg("--help")
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        }
    }
    fs::remove_file(codex.join("skills/agent-peer")).unwrap();
    fs::remove_file(claude.join("skills/agent-peer")).unwrap();
    fs::create_dir(claude.join("skills/agent-peer")).unwrap();
    fs::write(claude.join("skills/agent-peer/SKILL.md"), "user-maintained").unwrap();
    assert!(!install(&new_installer).status.success());
    assert!(!codex.join("skills/agent-peer").exists());
    assert_eq!(
        fs::read_to_string(claude.join("skills/agent-peer/SKILL.md")).unwrap(),
        "user-maintained"
    );
    assert!(old.join("share/agent-peer/SKILL.md").is_file());
}

fn main() -> Result<()> {
    if let Ok(mode) = env::var("AGENT_PEER_FIXTURE") {
        return fixture(&mode);
    }
    for (name, test) in [
        ("protocols", protocols as fn()),
        ("failures", failures),
        ("paths and validation", paths_and_validation),
        ("cancellation", cancellation),
        ("installation", installation),
    ] {
        print!("{name} ... ");
        std::io::stdout().flush().context("flush test status")?;
        test();
        println!("ok");
    }
    Ok(())
}
