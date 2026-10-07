use super::*;
use crate::host_rpc::connections::Connections;
use agent_domain::ThreadId;
use agent_protocol::models::ProjectScriptIcon;
use agent_protocol::operations::TerminalStatus;
use std::sync::Mutex;
use std::time::Duration;

fn script(id: &str, command: &str, setup: bool) -> ProjectScript {
    ProjectScript {
        id: id.into(),
        name: id.into(),
        command: command.into(),
        icon: ProjectScriptIcon::Configure,
        run_on_worktree_create: setup,
        run_async: None,
        preview_url: None,
        auto_open_preview: None,
    }
}

fn lines() -> (SetupProgress, Arc<Mutex<Vec<String>>>) {
    let lines = Arc::new(Mutex::new(vec![]));
    let seen = lines.clone();
    let progress = SetupProgress::new(move |event| {
        if let SetupEvent::Output(line) = event {
            seen.lock().unwrap().push(line);
        }
    });
    (progress, lines)
}

#[test]
fn the_first_script_run_on_worktree_creation_is_the_setup() {
    let scripts = [
        script("lint", "vp lint", false),
        script("setup", "vp install", true),
        script("seed", "vp seed", true),
    ];
    assert_eq!(setup_script(&scripts).unwrap().id, "setup");
    assert_eq!(setup_script(&scripts[..1]), None);
}

// ProjectSetupScriptRunner.ts stripTerminalControl and the output line filter.
#[test]
fn output_lines_drop_terminal_control_and_stay_bounded() {
    assert_eq!(clean("\x1b[32mok\x1b[0m done \x07  "), "ok done");
    assert_eq!(clean("\x1b]0;title\x07"), "");
    assert_eq!(bounded(&"x".repeat(500)).len(), 400);
}

// ProjectSetupScriptRunner.ts wrapCommandForCompletion and the sentinel pattern.
#[test]
fn wrapped_commands_report_their_exit_code_after_the_block() {
    let sentinel = "__SETUP_DONE___token:";
    assert_eq!(
        wrap_command("vp install\n# done", CompletionShell::Posix, sentinel),
        "( vp install\r# done\r); printf '\\n__SETUP_DONE___token:%s\\n' \"$?\""
    );
    assert_eq!(
        wrap_command("vp install", CompletionShell::Fish, sentinel),
        "begin\rvp install\rend; printf '\\n__SETUP_DONE___token:%s\\n' $status"
    );
    assert!(
        wrap_command("vp install", CompletionShell::PowerShell, sentinel)
            .starts_with("$global:LASTEXITCODE = $null; & {\rvp install\r};")
    );
    assert_eq!(
        completion_shell(false, Some("/opt/homebrew/bin/fish")),
        CompletionShell::Fish
    );
    assert_eq!(
        completion_shell(false, Some("/bin/zsh")),
        CompletionShell::Posix
    );
    assert_eq!(completion_shell(false, None), CompletionShell::Posix);
    assert_eq!(
        completion_shell(true, Some("/bin/zsh")),
        CompletionShell::PowerShell
    );
    assert_eq!(
        sentinel_code("__SETUP_DONE___token:-2", sentinel),
        Some(Some(-2))
    );
    assert_eq!(
        sentinel_code("x__SETUP_DONE___token:0\r", sentinel),
        Some(Some(0))
    );
    assert_eq!(sentinel_code("__SETUP_DONE___other:0", sentinel), None);
    assert_eq!(sentinel_code("__SETUP_DONE___token:", sentinel), None);
}

// ProjectSetupScriptRunner.test.ts: redrawn progress lines become lines of their
// own and a closed terminal settles the completion.
#[tokio::test]
async fn observed_output_splits_redraws_and_ends_with_the_terminal() {
    let (progress, seen) = lines();
    let (sender, receiver) = mpsc::unbounded_channel();
    let observed = tokio::spawn(observe(
        receiver,
        "__SETUP_DONE___t:".into(),
        vec![],
        progress,
    ));
    sender
        .send(TerminalOutput::Data(
            "Downloading 10%\rDownloading 20%\r\nDone\n"
                .as_bytes()
                .to_vec(),
        ))
        .unwrap();
    // A character split across chunks stays whole.
    sender.send(TerminalOutput::Data(vec![0xe6, 0x97])).unwrap();
    sender
        .send(TerminalOutput::Data(vec![0xa5, b'\n']))
        .unwrap();
    sender.send(TerminalOutput::Closed).unwrap();
    assert_eq!(observed.await.unwrap(), None);
    assert_eq!(
        *seen.lock().unwrap(),
        ["Downloading 10%", "Downloading 20%", "Done", "日"]
    );
}

#[tokio::test]
async fn the_sentinel_settles_and_the_wrapper_echo_is_hidden() {
    let (progress, seen) = lines();
    let (sender, receiver) = mpsc::unbounded_channel();
    let wrapper = wrap_command("vp install", CompletionShell::Posix, "__SETUP_DONE___t:");
    let echoed = wrapper.split('\r').map(str::to_owned).collect();
    let observed = tokio::spawn(observe(
        receiver,
        "__SETUP_DONE___t:".into(),
        echoed,
        progress,
    ));
    sender
        .send(TerminalOutput::Data(
            format!(
                "% {}\r\ninstalled\r\n\r\n__SETUP_DONE___t:3\r\n",
                wrapper.replace('\r', "\r\n")
            )
            .into_bytes(),
        ))
        .unwrap();
    assert_eq!(observed.await.unwrap(), Some(3));
    assert_eq!(*seen.lock().unwrap(), ["installed"]);
}

#[test]
fn setup_terminals_get_the_project_paths_without_color() {
    assert_eq!(
        setup_environment("/repo", "/repo-worktree"),
        BTreeMap::from(
            [
                ("PROJECT_ROOT", "/repo"),
                ("WORKTREE_PATH", "/repo-worktree"),
                ("COLORTERM", ""),
                ("NO_COLOR", "1"),
                ("FORCE_COLOR", "0"),
            ]
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
        )
    );
}

#[cfg(unix)]
fn request(thread: &ThreadId, cwd: &str, observe: SetupProgress) -> SetupRequest {
    SetupRequest {
        thread: thread.clone(),
        project: "project".into(),
        project_root: "/repo".into(),
        cwd: cwd.into(),
        observe,
    }
}

// ProjectSetupScriptRunner.ts: the script runs in the thread's setup terminal in
// the worktree with the project paths and without color; a clean run closes
// the idle shell, a failed one keeps it.
#[cfg(unix)]
#[tokio::test]
async fn setup_runs_in_the_threads_terminal_and_reports_its_exit_code() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let worktree = dunce::canonicalize(directory.path()).unwrap();
        let cwd = worktree.to_str().unwrap();
        let thread = ThreadId::new("thread-1").unwrap();
        // Nothing else runs in the shell; a real snapshot could race its prompt.
        let history = tempfile::tempdir().unwrap();
        let terminals = Arc::new(Terminals::with_processes(
            Connections::new(),
            Arc::new(|| Box::pin(async { Ok(Default::default()) })),
            Duration::from_secs(3600),
            history.path().to_path_buf(),
        ));
        let command = "printf '%s|%s|%s|%s|%s|%s' \"$PWD\" \"$PROJECT_ROOT\" \
                       \"$WORKTREE_PATH\" \"${COLORTERM-unset}\" \"$NO_COLOR\" \
                       \"$FORCE_COLOR\" > environment.txt; echo configured";
        let (progress, seen) = lines();
        let started = run(
            &terminals,
            &request(&thread, cwd, progress),
            &script("setup", command, true),
        )
        .await
        .unwrap();
        assert_eq!(started.terminal_id, "setup-setup");
        assert!(started.run_async);
        assert_eq!(started.completion.unwrap().await, Some(0));
        assert_eq!(
            std::fs::read_to_string(worktree.join("environment.txt")).unwrap(),
            format!("{cwd}|/repo|{cwd}||1|0")
        );
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .any(|line| line.ends_with("configured"))
        );
        assert!(
            terminals.summaries_now().is_empty(),
            "a clean setup closes its shell"
        );

        let (progress, _) = lines();
        let failed = run(
            &terminals,
            &request(&thread, cwd, progress),
            &script("setup", "exit 3", true),
        )
        .await
        .unwrap();
        assert_eq!(failed.completion.unwrap().await, Some(3));
        let kept = terminals.summaries_now();
        assert_eq!(kept.len(), 1);
        assert_eq!(
            (kept[0].terminal_id.as_str(), kept[0].status),
            ("setup-setup", TerminalStatus::Running)
        );
        // An unobserved run types the command as is and returns no completion.
        let unobserved = run(
            &terminals,
            &request(&thread, cwd, SetupProgress::default()),
            &script("setup", "touch unobserved", true),
        )
        .await
        .unwrap();
        assert!(unobserved.completion.is_none());
        let marker = worktree.join("unobserved");
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        terminals.close_thread(&thread).await;
        assert!(terminals.summaries_now().is_empty());
    })
    .await
    .expect("setup stalled");
}
