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
    history: &Path,
) -> Terminals {
    Terminals::with_processes(
        router,
        Arc::new(move || {
            calls.fetch_add(1, Ordering::AcqRel);
            let table = table.lock().unwrap().clone();
            Box::pin(async move { table })
        }),
        interval,
        history.to_path_buf(),
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

// Manager.ts windowsProcessTableSnapshot: `pid|ppid|name` lines, where a
// process needs a positive id.
#[test]
fn windows_listings_name_each_shells_command() {
    let table = ProcessTable::parse_windows(
        "10|1|pwsh.exe\r\n 20 | 10 |node.exe|extra\r\n0|0|System Idle Process\r\nbad|1|x\r\n30|10|\r\n",
    );
    let mut expected = ProcessTable::default();
    expected.insert(10, 1, "pwsh.exe");
    expected.insert(20, 10, "node.exe");
    expected.insert(30, 10, "");
    assert_eq!(table, expected);
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
    let history = tempfile::tempdir().unwrap();
    let terminals = terminals_with(
        Connections::new(),
        table.clone(),
        calls.clone(),
        Duration::from_millis(20),
        history.path(),
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
        .close(&thread_terminal_handle_for("thread-1", "term-1"), false)
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
        let history = tempfile::tempdir().unwrap();
        let terminals = Terminals::new(router.clone(), history.path().to_path_buf());
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
        let history = tempfile::tempdir().unwrap();
        let terminals = Terminals::new(router.clone(), history.path().to_path_buf());
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
                    delete_history: true,
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
        let history = tempfile::tempdir().unwrap();
        let terminals = Terminals::new(Connections::new(), history.path().to_path_buf());
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
    let history = tempfile::tempdir().unwrap();
    let terminals = terminals_with(
        Connections::new(),
        table.clone(),
        Arc::default(),
        Duration::from_secs(3600),
        history.path(),
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

fn contains(data: &[u8], text: &str) -> bool {
    data.windows(text.len())
        .any(|window| window == text.as_bytes())
}

/// Output the session receives until `text` appears in it.
async fn output_until(session: &mut crate::host_rpc::connections::HostSession, text: &str) {
    let seen = Mutex::new(Vec::new());
    notifications_until(session, |event| {
        if let Notification::Output { data, .. } = event {
            seen.lock().unwrap().extend_from_slice(data);
        }
        contains(&seen.lock().unwrap(), text)
    })
    .await;
}

/// The screen the session is given next.
async fn restored(session: &mut crate::host_rpc::connections::HostSession) -> Vec<u8> {
    let seen = notifications_until(session, |event| {
        matches!(event, Notification::TerminalRestored { .. })
    })
    .await;
    match seen.last() {
        Some(Notification::TerminalRestored { data, .. }) => data.clone(),
        _ => unreachable!(),
    }
}

// Manager.test.ts "caps persisted history to configured line limit" and "strips
// replay-unsafe terminal query and reply sequences from persisted history": a
// terminal keeps its newest 5000 lines, and its kept screen asks nothing when
// it is restored.
#[test]
fn a_kept_screen_is_bounded_and_asks_nothing_when_restored() {
    use alacritty_terminal::vte::ansi::Processor;
    let size = TerminalSize { cols: 40, rows: 10 };
    let replies = Replies::default();
    let mut original = new_screen(size, &replies);
    let mut parser: Processor = Default::default();
    let mut output = String::new();
    for line in 1..=6_000 {
        output.push_str(&format!("line-{line}\r\n"));
    }
    output.push_str("\x1b[c\x1b[6n\x1b]10;?\x1b\\\x1b[>q\x1bP$qm\x1b\\done");
    parser.advance(&mut original, output.as_bytes());
    assert!(!replies.0.lock().unwrap().is_empty());
    let kept = original.ansi_checkpoint(None);

    let replies = Replies::default();
    let mut restored = new_screen(size, &replies);
    let mut reader: Processor = Default::default();
    reader.advance(&mut restored, &kept);
    assert!(replies.0.lock().unwrap().is_empty());
    assert_eq!(restored.grid().history_size(), SCROLLBACK_LINES);
    let text = |term: &alacritty_terminal::Term<Replies>| {
        use alacritty_terminal::index::{Column, Line};
        let lines = term.grid().history_size() as i32;
        (-lines..size.rows as i32)
            .map(|row| {
                (0..size.cols as usize)
                    .map(|col| term.grid()[Line(row)][Column(col)].c)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
    };
    let lines = text(&restored);
    assert_eq!(lines, text(&original));
    assert!(!lines.iter().any(|line| line == "line-1"));
    assert!(lines.iter().any(|line| line == "line-6000"));
    assert_eq!(lines.last().map(String::as_str), Some("done"));
}

// Manager.test.ts "reports a missing cwd without an artificial cause", "reports
// a cwd that is not a directory" and "preserves non-notFound cwd stat failures".
#[cfg(unix)]
#[tokio::test]
async fn a_terminal_needs_a_directory_it_can_reach() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing");
    let missing = missing.to_str().unwrap();
    assert_eq!(
        directory(missing).await.unwrap_err(),
        format!("Terminal cwd does not exist: {missing}")
    );
    let file = root.path().join("file");
    std::fs::write(&file, b"").unwrap();
    let file = file.to_str().unwrap();
    assert_eq!(
        directory(file).await.unwrap_err(),
        format!("Terminal cwd is not a directory: {file}")
    );
    let locked = root.path().join("locked");
    std::fs::create_dir_all(locked.join("inner")).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let inner = locked.join("inner");
    let inner = inner.to_str().unwrap();
    let denied = directory(inner).await;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        denied.unwrap_err(),
        format!("Failed to access terminal cwd: {inner}")
    );
}

// Manager.test.ts "bounds persisted and attached history without truncating
// live output" and the history read on first open: a terminal opened again
// after the Host restarts shows its kept screen; closing it with its history
// deletes that.
#[cfg(unix)]
#[tokio::test]
async fn a_terminal_shows_its_kept_screen_after_the_host_restarts() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let history = tempfile::tempdir().unwrap();
        let cwd = directory.path().to_string_lossy().into_owned();
        let owner = thread("thread-1");
        let handle = thread_terminal_handle_for("thread-1", "term-1");
        let router = Connections::new();
        let terminals = Terminals::new(router.clone(), history.path().to_path_buf());
        let mut session = router.open_authenticated_session(Some("phone".into()));
        terminals
            .attach(session.id(), &start(&owner, "term-1", Some(&cwd)))
            .await
            .unwrap();
        terminals
            .request(session.id(), &write(&handle, "echo kept-$((40 + 2))\n"))
            .await
            .unwrap();
        output_until(&mut session, "kept-42").await;
        terminals.shutdown().await;
        let file = terminals.inner.history.path(&owner, "term-1");
        assert!(contains(&std::fs::read(&file).unwrap(), "kept-42"));
        drop(terminals);

        let router = Connections::new();
        let terminals = Terminals::new(router.clone(), history.path().to_path_buf());
        let mut session = router.open_authenticated_session(Some("phone".into()));
        terminals
            .attach(session.id(), &start(&owner, "term-1", Some(&cwd)))
            .await
            .unwrap();
        assert!(contains(&restored(&mut session).await, "kept-42"));
        assert_eq!(terminals.summaries_now()[0].status, TerminalStatus::Running);
        // An unknown terminal of the same thread starts empty.
        terminals
            .attach(session.id(), &start(&owner, "term-2", Some(&cwd)))
            .await
            .unwrap();
        assert!(!contains(&restored(&mut session).await, "kept-42"));
        terminals
            .request(
                session.id(),
                &Call::KillTerminal(TerminalKill {
                    process_handle: handle.clone(),
                    delete_history: true,
                }),
            )
            .await
            .unwrap();
        assert!(!file.exists());
        terminals.shutdown().await;
    })
    .await
    .expect("terminal history stalled");
}

// Manager.test.ts "clears transcript and emits cleared event": clearing empties
// the kept screen and every attached screen while the shell keeps running; an
// exited terminal clears too.
#[cfg(unix)]
#[tokio::test]
async fn clearing_a_terminal_empties_its_screens_and_history() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let history = tempfile::tempdir().unwrap();
        let cwd = directory.path().to_string_lossy().into_owned();
        let owner = thread("thread-1");
        let handle = thread_terminal_handle_for("thread-1", "term-1");
        let router = Connections::new();
        let terminals = Terminals::new(router.clone(), history.path().to_path_buf());
        let mut session = router.open_authenticated_session(Some("phone".into()));
        terminals
            .attach(session.id(), &start(&owner, "term-1", Some(&cwd)))
            .await
            .unwrap();
        terminals
            .request(session.id(), &write(&handle, "echo before-$((1 + 1))\n"))
            .await
            .unwrap();
        output_until(&mut session, "before-2").await;
        let pid = terminals.summaries_now()[0].pid;
        let clear = Call::ClearTerminal(ClearTerminal {
            thread: owner.clone(),
            terminal_id: "term-1".into(),
        });
        terminals.request(session.id(), &clear).await.unwrap();
        assert!(!contains(&restored(&mut session).await, "before-2"));
        let file = terminals.inner.history.path(&owner, "term-1");
        assert!(!contains(&std::fs::read(&file).unwrap(), "before-2"));
        assert_eq!(terminals.summaries_now()[0].pid, pid);
        // A new screen still follows the shell.
        terminals
            .request(session.id(), &write(&handle, "echo after-$((2 + 1))\n"))
            .await
            .unwrap();
        output_until(&mut session, "after-3").await;
        terminals
            .request(session.id(), &write(&handle, "exit\n"))
            .await
            .unwrap();
        notifications_until(&mut session, |event| {
            matches!(event, Notification::Exited { .. })
        })
        .await;
        until("the terminal exited", || {
            terminals.summaries_now()[0].status == TerminalStatus::Exited
        })
        .await;
        assert!(contains(&std::fs::read(&file).unwrap(), "after-3"));
        terminals.request(session.id(), &clear).await.unwrap();
        assert!(!contains(&restored(&mut session).await, "after-3"));
        assert!(!file.exists());
        terminals
            .attach(session.id(), &start(&owner, "term-1", None))
            .await
            .unwrap();
        assert!(!contains(&restored(&mut session).await, "after-3"));
        let unknown = terminals
            .request(
                session.id(),
                &Call::ClearTerminal(ClearTerminal {
                    thread: owner.clone(),
                    terminal_id: "term-9".into(),
                }),
            )
            .await
            .unwrap_err();
        assert_eq!(
            unknown,
            "Unknown terminal thread: thread-1, terminal: term-9"
        );
        terminals.shutdown().await;
    })
    .await
    .expect("terminal clear stalled");
}

// Manager.test.ts "restarts terminal with empty transcript and respawns pty":
// a running terminal restarts in place with a new shell and an empty history.
#[cfg(unix)]
#[tokio::test]
async fn restarting_a_running_terminal_starts_a_new_shell_with_an_empty_history() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let history = tempfile::tempdir().unwrap();
        let cwd = directory.path().to_string_lossy().into_owned();
        let owner = thread("thread-1");
        let handle = thread_terminal_handle_for("thread-1", "term-1");
        let router = Connections::new();
        let terminals = Terminals::new(router.clone(), history.path().to_path_buf());
        let (_, mut metadata) = terminals.subscribe_metadata();
        let mut session = router.open_authenticated_session(Some("phone".into()));
        terminals
            .attach(session.id(), &start(&owner, "term-1", Some(&cwd)))
            .await
            .unwrap();
        terminals
            .request(session.id(), &write(&handle, "echo old-$((3 + 4))\n"))
            .await
            .unwrap();
        output_until(&mut session, "old-7").await;
        let pid = terminals.summaries_now()[0].pid;
        let restart = |cwd: &Path| {
            Call::RestartTerminal(RestartTerminal {
                thread: owner.clone(),
                terminal_id: "term-1".into(),
                cwd: cwd.to_string_lossy().into_owned(),
                worktree_path: None,
                size: TerminalSize { cols: 90, rows: 20 },
                env: BTreeMap::from([("RESTARTED".into(), "yes".into())]),
            })
        };
        terminals
            .request(session.id(), &restart(other.path()))
            .await
            .unwrap();
        let screen = restored(&mut session).await;
        assert!(!contains(&screen, "old-7"));
        let restarted = terminals.summaries_now()[0].clone();
        assert_ne!(restarted.pid, pid);
        assert_eq!(restarted.status, TerminalStatus::Running);
        assert_eq!(
            Path::new(&restarted.cwd),
            dunce::canonicalize(other.path()).unwrap()
        );
        loop {
            if let TerminalMetadataEvent::Upsert { terminal } = metadata.recv().await.unwrap()
                && terminal.pid == restarted.pid
                && terminal.status == TerminalStatus::Running
            {
                break;
            }
        }
        terminals
            .request(
                session.id(),
                &write(&handle, "printf '%s' \"$RESTARTED\" > restarted\n"),
            )
            .await
            .unwrap();
        until("the new shell has the new environment", || {
            std::fs::read_to_string(other.path().join("restarted"))
                .ok()
                .as_deref()
                == Some("yes")
        })
        .await;
        terminals.shutdown().await;
        let file = terminals.inner.history.path(&owner, "term-1");
        assert!(!contains(&std::fs::read(&file).unwrap(), "old-7"));
        // Restarting a terminal the Host does not have opens it.
        terminals
            .request(
                session.id(),
                &Call::RestartTerminal(RestartTerminal {
                    terminal_id: "term-2".into(),
                    ..match restart(directory.path()) {
                        Call::RestartTerminal(params) => params,
                        _ => unreachable!(),
                    }
                }),
            )
            .await
            .unwrap();
        assert!(
            terminals
                .summaries_now()
                .iter()
                .any(|terminal| terminal.terminal_id == "term-2"
                    && terminal.status == TerminalStatus::Running)
        );
        terminals.shutdown().await;
    })
    .await
    .expect("terminal restart stalled");
}

// Manager.test.ts "deletes history file when close(deleteHistory=true)" and the
// cleanup of a thread: an idle close keeps the history, a thread's cleanup
// deletes all of it and no other thread's.
#[cfg(unix)]
#[tokio::test]
async fn closing_keeps_history_unless_asked_and_thread_cleanup_deletes_it() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let history = tempfile::tempdir().unwrap();
        let table = Arc::new(Mutex::new(Ok(ProcessTable::default())));
        let terminals = terminals_with(
            Connections::new(),
            table,
            Arc::default(),
            Duration::from_secs(3600),
            history.path(),
        );
        for (owner, id) in [
            ("thread", "term-1"),
            ("thread", "term-2"),
            ("thread-2", "term-1"),
        ] {
            terminals
                .open(OpenTerminal {
                    thread: thread(owner),
                    terminal_id: id.into(),
                    cwd: directory.path().to_string_lossy().into_owned(),
                    worktree_path: None,
                    env: BTreeMap::new(),
                    size: None,
                })
                .await
                .unwrap();
        }
        let file = |owner: &str, id: &str| terminals.inner.history.path(&thread(owner), id);
        terminals
            .close_idle(&thread("thread"), Some("term-1"))
            .await;
        assert!(file("thread", "term-1").exists());
        terminals
            .close(&thread_terminal_handle_for("thread", "term-2"), true)
            .await
            .unwrap();
        assert!(!file("thread", "term-2").exists());
        terminals.close_thread(&thread("thread")).await;
        assert!(!file("thread", "term-1").exists());
        terminals.shutdown().await;
        assert!(file("thread-2", "term-1").exists());
    })
    .await
    .expect("terminal close stalled");
}
