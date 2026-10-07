use super::*;
use agent_protocol::operations::{DetachTerminal, TerminalKill, TerminalWrite};
use std::sync::atomic::AtomicUsize;

fn thread(id: &str) -> ThreadId {
    ThreadId::new(id).unwrap()
}

fn start(thread: &ThreadId, terminal_id: &str, cwd: Option<&str>) -> StartTerminal {
    StartTerminal {
        thread: thread.clone(),
        terminal_id: terminal_id.into(),
        cwd: cwd.map(str::to_owned),
        worktree_path: None,
        size: TerminalSize { cols: 80, rows: 24 },
        env: BTreeMap::new(),
        restart_if_not_running: false,
    }
}

fn write(handle: &str, text: &str) -> Call {
    Call::WriteTerminal(TerminalWrite {
        process_handle: handle.into(),
        data: text.as_bytes().to_vec(),
    })
}

/// A Host whose process snapshots come from `table`.
fn terminals_with(
    router: Connections,
    table: Arc<Mutex<Result<ProcessTable, String>>>,
    calls: Arc<AtomicUsize>,
    interval: Duration,
) -> Terminals {
    Terminals::with_processes(
        router,
        Arc::new(move || {
            calls.fetch_add(1, Ordering::AcqRel);
            let table = table.lock().unwrap().clone();
            Box::pin(async move { table })
        }),
        interval,
    )
}

async fn until(what: &str, check: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !check() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn notifications_until(
    session: &mut crate::host_rpc::connections::HostSession,
    found: impl Fn(&Notification) -> bool,
) -> Vec<Notification> {
    let mut seen = vec![];
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(frame) = session.recv().await {
            let notification = agent_protocol::protocol::decode::<Notification>(&frame).unwrap();
            let done = found(&notification);
            seen.push(notification);
            if done {
                return;
            }
        }
    })
    .await
    .expect("notification arrives");
    seen
}

#[test]
fn checkpoint_restores_screen_modes_and_split_sequences() {
    use alacritty_terminal::{Term, term::Config, vte::ansi::Processor};
    let size = TerminalSize { rows: 5, cols: 12 };
    let basic = [
        (
            b"hello\r\nworld\x1b[31m!\x1b[3;8H".as_slice(),
            b"again".as_slice(),
        ),
        (
            b"original\x1b[?1049h\x1b[2;4r\x1b[?6hALT\x1b[?2004h",
            b"\r\nmore\x1b[?1049l!",
        ),
        (b"012345678901", b"next"),
        (b"hi\x1b[38;2;12;", b"34;56mcolor"),
        (b"\xe6\x97", b"\xa5\xe6\x9c\xac"),
        (b"abc\x1b[2J", b"\x1b[3b"),
        (b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix", b"\r\nseven"),
    ];
    let sequences = [
        "日本語\r\n12345678901日\r\ne\u{301}\x1b[31;44;1mred\x1b[0m",
        "abc\x1b7\r\nother\x1b8!",
        "123456789012\x1b7\r\nnext\x1b8X",
        "screen\x1b[?1049h\x1b[2;4r\x1b[?6hALT\x1b[?1049l!",
        "abc\x1b]0;title\x1b\\hello\x1b[38;2;12;34;56mRGB",
        "before\x1b[?2026hupdate\r\nmore\x1b[?2026lafter",
    ];
    let cases = basic.into_iter().chain(
        sequences
            .iter()
            .flat_map(|text| (0..=text.len()).map(move |index| text.as_bytes().split_at(index))),
    );
    for (before, after) in cases {
        let mut original = Term::new(Config::default(), &Dimensions(size), Replies::default());
        let mut parser: Processor = Default::default();
        parser.advance(&mut original, before);
        let mut restored = Term::new(Config::default(), &Dimensions(size), Replies::default());
        let mut reader: Processor = Default::default();
        let mut checkpoint = original.ansi_checkpoint(parser.preceding_char());
        checkpoint.extend(parser.checkpoint_tail());
        reader.advance(&mut restored, &checkpoint);
        parser.advance(&mut original, after);
        reader.advance(&mut restored, after);
        assert_eq!(original.mode(), restored.mode(), "{before:?}");
        assert_eq!(
            original.grid().cursor.point,
            restored.grid().cursor.point,
            "{before:?}"
        );
        assert_eq!(
            original.grid().history_size(),
            restored.grid().history_size(),
            "{before:?}"
        );
        for row in -(original.grid().history_size() as i32)..5 {
            for col in 0..12 {
                use alacritty_terminal::index::{Column, Line};
                let a = &original.grid()[Line(row)][Column(col)];
                let b = &restored.grid()[Line(row)][Column(col)];
                assert_eq!(
                    (a.c, a.fg, a.bg, a.flags, a.zerowidth()),
                    (b.c, b.fg, b.bg, b.flags, b.zerowidth()),
                    "{before:?}, {row}:{col}"
                );
            }
        }
    }
}

// terminalLabels.ts getTerminalLabel.
#[test]
fn tab_labels_number_term_ids_and_keep_other_ids() {
    assert_eq!(terminal_label("term-1"), "Terminal 1");
    assert_eq!(terminal_label("Terminal-12"), "Terminal 12");
    assert_eq!(terminal_label("setup-bootstrap"), "setup-bootstrap");
    assert_eq!(terminal_label("term-"), "term-");
}

// Manager.test.ts "derives subprocess activity for every terminal from one
// shared process snapshot".
#[test]
fn one_process_snapshot_names_each_shells_command() {
    let table =
        ProcessTable::parse("  100  9000 vim\n  101   100 git\n  200  9001 /usr/bin/python3\n");
    assert_eq!(
        table.subprocess(9000),
        Subprocess::Running(Some("vim".into()))
    );
    assert_eq!(
        table.subprocess(9001),
        Subprocess::Running(Some("python3".into()))
    );
    assert_eq!(table.subprocess(9002), Subprocess::Idle);
    assert_eq!(
        processes::command_name("[kworker/0:1]").as_deref(),
        Some("0:1")
    );
    assert_eq!(
        processes::command_name("(sd-pam)").as_deref(),
        Some("sd-pam")
    );
    assert_eq!(processes::command_name("  "), None);
}

// Manager.test.ts "closes only a thread's idle shells, ignoring a helper forked
// from the shell": the shell decisions behind closeIdle.
#[test]
fn a_childless_copy_of_the_shell_is_not_a_command() {
    let mut table = ProcessTable::default();
    table.insert(10, 1, "zsh");
    table.insert(11, 10, "zsh");
    table.insert(20, 1, "zsh");
    table.insert(21, 20, "node");
    table.insert(30, 1, "zsh");
    table.insert(31, 30, "zsh");
    table.insert(32, 31, "sleep");
    assert_eq!(table.subprocess(10), Subprocess::Idle);
    assert_eq!(
        table.subprocess(20),
        Subprocess::Running(Some("node".into()))
    );
    assert_eq!(
        table.subprocess(30),
        Subprocess::Running(Some("zsh".into()))
    );
}

// Manager.test.ts "calculates snapshot failure backoff and success reset delays".
#[test]
fn failed_snapshots_back_off_up_to_a_minute() {
    let interval = Duration::from_millis(1000);
    let delays: Vec<u128> = [0, 1, 2, 30]
        .into_iter()
        .map(|failures| poll_delay(interval, failures).as_millis())
        .collect();
    assert_eq!(delays, [1000, 2000, 4000, 60000]);
}

// Manager.test.ts "filters app runtime env variables from terminal sessions",
// "injects runtime env overrides into spawned terminals" and "expands provider
// home paths passed to setup terminals".
#[test]
fn shells_drop_host_variables_and_take_the_callers() {
    let base = [
        ("PORT", "5173"),
        ("BEX_BROWSER_EXECUTABLE", "/browser"),
        ("VITE_DEV_SERVER_URL", "http://localhost"),
        ("TEST_TERMINAL_KEEP", "keep-me"),
        ("FORCE_COLOR", "3"),
    ]
    .map(|(key, value)| (key.to_owned(), value.to_owned()));
    let overlay = BTreeMap::from([
        ("FORCE_COLOR".to_owned(), "0".to_owned()),
        ("CODEX_HOME".to_owned(), "~/.codex-work".to_owned()),
        ("VITE_FROM_CALLER".to_owned(), "kept".to_owned()),
    ]);
    let env = environment(base, &overlay, Some(Path::new("/home/user")));
    assert_eq!(env.get("PORT"), None);
    assert_eq!(env.get("BEX_BROWSER_EXECUTABLE"), None);
    assert_eq!(env.get("VITE_DEV_SERVER_URL"), None);
    assert_eq!(env["TEST_TERMINAL_KEEP"], "keep-me");
    assert_eq!(env["FORCE_COLOR"], "0");
    assert_eq!(env["VITE_FROM_CALLER"], "kept");
    assert_eq!(env["CODEX_HOME"], "/home/user/.codex-work");
    assert_eq!(env["COLORTERM"], "truecolor");
    let plain = BTreeMap::from([("COLORTERM".to_owned(), String::new())]);
    assert_eq!(environment([], &plain, None)["COLORTERM"], "");
}

#[test]
fn requests_validate_ids_sizes_and_environment() {
    let valid = start(&thread("thread"), "term-1", Some("/tmp"));
    assert!(valid.validate().is_ok());
    let wide = StartTerminal {
        size: TerminalSize {
            cols: 423,
            rows: 24,
        },
        ..valid.clone()
    };
    assert!(wide.validate().is_ok());
    for invalid in [
        StartTerminal {
            size: TerminalSize { cols: 10, rows: 0 },
            ..valid.clone()
        },
        StartTerminal {
            terminal_id: " ".into(),
            ..valid.clone()
        },
        StartTerminal {
            env: BTreeMap::from([("bad-key".into(), "x".into())]),
            ..valid.clone()
        },
    ] {
        assert!(invalid.validate().is_err(), "{invalid:?}");
    }
    assert_eq!(valid.handle(), "terminal:thread:term-1");
}

// Manager.test.ts "does not invoke subprocess polling until a terminal session
// is running" and "emits subprocess activity events when child-process state
// changes".
#[cfg(unix)]
#[tokio::test]
async fn running_shells_report_their_command_on_the_metadata_stream() {
    let directory = tempfile::tempdir().unwrap();
    let table = Arc::new(Mutex::new(Ok(ProcessTable::default())));
    let calls = Arc::new(AtomicUsize::new(0));
    let terminals = terminals_with(
        Connections::new(),
        table.clone(),
        calls.clone(),
        Duration::from_millis(20),
    );
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(calls.load(Ordering::Acquire), 0);
    let (snapshot, mut events) = terminals.subscribe_metadata();
    assert!(snapshot.is_empty());
    let owner = thread("thread-1");
    terminals
        .open(OpenTerminal {
            thread: owner.clone(),
            terminal_id: "term-1".into(),
            cwd: directory.path().to_string_lossy().into_owned(),
            worktree_path: None,
            env: BTreeMap::new(),
            size: None,
        })
        .await
        .unwrap();
    let running = loop {
        match events.recv().await.unwrap() {
            TerminalMetadataEvent::Upsert { terminal }
                if terminal.status == TerminalStatus::Running =>
            {
                break terminal;
            }
            _ => {}
        }
    };
    assert_eq!(running.label, "Terminal 1");
    assert!(!running.has_running_subprocess);
    let pid = running.pid.unwrap();
    let mut busy = ProcessTable::default();
    busy.insert(pid + 100_000, pid, "vim");
    *table.lock().unwrap() = Ok(busy);
    let activity = loop {
        if let TerminalMetadataEvent::Upsert { terminal } = events.recv().await.unwrap() {
            break terminal;
        }
    };
    assert!(calls.load(Ordering::Acquire) > 0);
    assert_eq!(
        (activity.has_running_subprocess, activity.label.as_str()),
        (true, "vim")
    );
    // A failed snapshot keeps the last known state.
    *table.lock().unwrap() = Err("ps failed".into());
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(terminals.summaries_now()[0].has_running_subprocess);
    *table.lock().unwrap() = Ok(ProcessTable::default());
    let idle = loop {
        if let TerminalMetadataEvent::Upsert { terminal } = events.recv().await.unwrap() {
            break terminal;
        }
    };
    assert_eq!(
        (idle.has_running_subprocess, idle.label.as_str()),
        (false, "Terminal 1")
    );
    terminals
        .close(&thread_terminal_handle_for("thread-1", "term-1"))
        .await
        .unwrap();
    loop {
        if let TerminalMetadataEvent::Remove {
            thread,
            terminal_id,
        } = events.recv().await.unwrap()
        {
            assert_eq!((thread, terminal_id.as_str()), (owner, "term-1"));
            break;
        }
    }
    assert!(terminals.summaries_now().is_empty());
}

// Manager.test.ts "attaches to running sessions without restarting them" and
// "supports multiple terminals per thread independently": any paired device
// attaches, detaching releases one session, and disconnecting stops nothing.
#[cfg(unix)]
#[tokio::test]
async fn devices_share_a_threads_terminals_across_disconnects() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path().to_string_lossy().into_owned();
        let router = Connections::new();
        let terminals = Terminals::new(router.clone());
        let owner = thread("thread-1");
        let first = router.open_authenticated_session(Some("phone".into()));
        for id in ["term-1", "term-2"] {
            terminals.attach(first.id(), &start(&owner, id, Some(&cwd))).await.unwrap();
        }
        let one = thread_terminal_handle_for("thread-1", "term-1");
        let two = thread_terminal_handle_for("thread-1", "term-2");
        terminals.request(first.id(), &write(&one, "RETAINED=survived\n")).await.unwrap();
        terminals
            .request(first.id(), &Call::DetachTerminal(DetachTerminal { handle: one.clone() }))
            .await
            .unwrap();
        terminals.request(first.id(), &write(&two, "true\n")).await.unwrap();
        router.close_session(first.id());
        terminals.close_session(first.id());
        assert_eq!(terminals.summaries_now().len(), 2);

        // Another device attaches without knowing the directory.
        let mut second = router.open_authenticated_session(Some("laptop".into()));
        terminals.attach(second.id(), &start(&owner, "term-1", None)).await.unwrap();
        notifications_until(&mut second, |event| {
            matches!(event, Notification::TerminalRestored { .. })
        })
        .await;
        terminals
            .request(
                second.id(),
                &write(&one, "printf '%s' \"$RETAINED\" > retained\n"),
            )
            .await
            .unwrap();
        until("the shell kept its variable", || {
            std::fs::read_to_string(directory.path().join("retained")).ok().as_deref()
                == Some("survived")
        })
        .await;
        terminals
            .request(
                second.id(),
                &write(&one, "stty -echo -icanon min 0 time 5; python3 -c 'import os; os.write(1,b\"\\x1b[6n\"*40); data=b\"\"\nwhile data.count(b\"R\")<40:\n part=os.read(0,4096)\n if not part: break\n data+=part\nopen(\"query-reply\",\"wb\").write(data)'; stty sane\n"),
            )
            .await
            .unwrap();
        until("the Host answered the terminal queries", || {
            std::fs::read(directory.path().join("query-reply")).is_ok_and(|bytes| {
                bytes.starts_with(b"\x1b[") && bytes.iter().filter(|byte| **byte == b'R').count() == 40
            })
        })
        .await;
        // An unknown terminal without a directory cannot be opened.
        assert!(terminals.attach(second.id(), &start(&owner, "term-3", None)).await.is_err());
        // Revoking a device detaches it; the shell keeps running.
        terminals.revoke_device("laptop");
        notifications_until(&mut second, |event| {
            matches!(event, Notification::TerminalDetached { .. })
        })
        .await;
        assert!(terminals.summaries_now().iter().all(|t| t.status == TerminalStatus::Running));
        terminals.shutdown().await;
    })
    .await
    .expect("terminal sharing stalled");
}

// Manager.test.ts "attaches to exited sessions without restarting them",
// "restarts inactive sessions from attach only when requested" and "restarts a
// running session when open is called with a different cwd".
#[cfg(unix)]
#[tokio::test]
async fn exited_terminals_stay_until_closed_and_restart_on_request() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let directory = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let cwd = directory.path().to_string_lossy().into_owned();
        let router = Connections::new();
        let terminals = Terminals::new(router.clone());
        let owner = thread("thread-1");
        let handle = thread_terminal_handle_for("thread-1", "term-1");
        let mut session = router.open_authenticated_session(Some("phone".into()));
        terminals
            .attach(session.id(), &start(&owner, "term-1", Some(&cwd)))
            .await
            .unwrap();
        terminals
            .request(session.id(), &write(&handle, "exit 3\n"))
            .await
            .unwrap();
        notifications_until(&mut session, |event| {
            matches!(event, Notification::Exited { code: 3, .. })
        })
        .await;
        until("the terminal is exited", || {
            terminals.summaries_now()[0].status == TerminalStatus::Exited
        })
        .await;
        assert_eq!(terminals.summaries_now()[0].exit_code, Some(3));
        // Writes to an exited terminal are ignored.
        terminals
            .request(session.id(), &write(&handle, "ignored\n"))
            .await
            .unwrap();
        terminals
            .attach(session.id(), &start(&owner, "term-1", Some(&cwd)))
            .await
            .unwrap();
        let replay = notifications_until(&mut session, |event| {
            matches!(event, Notification::Exited { .. })
        })
        .await;
        assert!(matches!(replay[0], Notification::TerminalRestored { .. }));
        assert_eq!(terminals.summaries_now()[0].status, TerminalStatus::Exited);
        terminals
            .attach(
                session.id(),
                &StartTerminal {
                    restart_if_not_running: true,
                    ..start(&owner, "term-1", Some(&cwd))
                },
            )
            .await
            .unwrap();
        assert_eq!(terminals.summaries_now()[0].status, TerminalStatus::Running);
        let pid = terminals.summaries_now()[0].pid;
        terminals
            .open(OpenTerminal {
                thread: owner.clone(),
                terminal_id: "term-1".into(),
                cwd: other.path().to_string_lossy().into_owned(),
                worktree_path: None,
                env: BTreeMap::new(),
                size: None,
            })
            .await
            .unwrap();
        let moved = &terminals.summaries_now()[0];
        assert_ne!(moved.pid, pid);
        assert_eq!(
            Path::new(&moved.cwd),
            dunce::canonicalize(other.path()).unwrap()
        );
        terminals
            .request(
                session.id(),
                &Call::KillTerminal(TerminalKill {
                    process_handle: handle.clone(),
                }),
            )
            .await
            .unwrap();
        notifications_until(&mut session, |event| {
            matches!(event, Notification::TerminalClosed { .. })
        })
        .await;
        assert!(terminals.summaries_now().is_empty());
    })
    .await
    .expect("terminal restart stalled");
}

// Manager.test.ts "closes all terminals for a thread when close omits
// terminalId": only that thread's terminals, never a thread sharing its prefix.
#[cfg(unix)]
#[tokio::test]
async fn thread_cleanup_closes_only_that_threads_terminals_and_their_jobs() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let directory = tempfile::tempdir().unwrap();
        let cwd = dunce::canonicalize(directory.path()).unwrap();
        let terminals = Terminals::new(Connections::new());
        for (owner, id) in [("thread", "term-1"), ("thread", "setup-x"), ("thread-2", "term-1")] {
            terminals
                .open(OpenTerminal {
                    thread: thread(owner),
                    terminal_id: id.into(),
                    cwd: cwd.to_string_lossy().into_owned(),
                    worktree_path: None,
                    env: BTreeMap::new(),
                    size: None,
                })
                .await
                .unwrap();
        }
        let handle = thread_terminal_handle_for("thread", "term-1");
        // Linux validation runs this Host with SHELL=/bin/sh (dash).
        // Disable interactive history expansion for Bash and Zsh on macOS.
        let command = "[ -z \"${BASH_VERSION-}\" ] || set +H\n[ -z \"${ZSH_VERSION-}\" ] || unsetopt BANG_HIST\nsleep 120 & first=$!; sleep 120 & printf '%s %s %s\\n' \"$$\" \"$first\" \"$!\" > owned-pids; wait\n";
        terminals.write(&handle, command.as_bytes().to_vec()).await.unwrap();
        let pids = loop {
            if let Ok(text) = std::fs::read_to_string(cwd.join("owned-pids"))
                && text.split_whitespace().count() == 3
            {
                break text;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert!(terminals.in_use(&cwd));
        terminals.close_thread(&thread("thread")).await;
        let left: Vec<_> = terminals
            .summaries_now()
            .into_iter()
            .map(|terminal| terminal.thread.to_string())
            .collect();
        assert_eq!(left, ["thread-2"]);
        for pid in pids.split_whitespace() {
            loop {
                let output = std::process::Command::new("ps").args(["-o", "stat=", "-p", pid]).output().unwrap();
                let state = String::from_utf8_lossy(&output.stdout);
                if state.trim().is_empty() || state.trim().starts_with('Z') {
                    break;
                }
                // macOS can report an exiting process as "?E" before it disappears.
                assert!(state.contains('E'), "process {pid} survived cleanup: {state}");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        terminals.shutdown().await;
        assert!(!terminals.in_use(&cwd));
    })
    .await
    .expect("terminal cleanup stalled");
}

// Manager.test.ts "keeps terminals that get input or output while closeIdle
// checks them" and the idle-shell case of closeIdle.
#[cfg(unix)]
#[tokio::test]
async fn close_idle_closes_quiet_shells_only() {
    let directory = tempfile::tempdir().unwrap();
    let table = Arc::new(Mutex::new(Ok(ProcessTable::default())));
    let terminals = terminals_with(
        Connections::new(),
        table.clone(),
        Arc::default(),
        Duration::from_secs(3600),
    );
    for id in ["idle", "busy"] {
        terminals
            .open(OpenTerminal {
                thread: thread("thread"),
                terminal_id: id.into(),
                cwd: directory.path().to_string_lossy().into_owned(),
                worktree_path: None,
                env: BTreeMap::new(),
                size: None,
            })
            .await
            .unwrap();
    }
    let busy = terminals
        .summaries_now()
        .into_iter()
        .find(|terminal| terminal.terminal_id == "busy")
        .unwrap();
    let mut running = ProcessTable::default();
    running.insert(busy.pid.unwrap() + 100_000, busy.pid.unwrap(), "node");
    *table.lock().unwrap() = Ok(running);
    terminals.close_idle(&thread("thread"), None).await;
    let left: Vec<_> = terminals
        .summaries_now()
        .into_iter()
        .map(|terminal| terminal.terminal_id)
        .collect();
    assert_eq!(left, ["busy"]);
    *table.lock().unwrap() = Err("ps failed".into());
    terminals.close_idle(&thread("thread"), None).await;
    assert_eq!(terminals.summaries_now().len(), 1);
    terminals.shutdown().await;
}
