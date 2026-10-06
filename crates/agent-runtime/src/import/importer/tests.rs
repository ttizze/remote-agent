use super::*;
use crate::actor::tests::{command_id, create, send};
use crate::import::scanner::tests::{
    HookFs, NOW, Setup, Temps, codex_session, ms, rollout, write_transcript,
};
use crate::import::{RecentThread, SessionMessage};
use crate::store::tests::{temp_store, thread};
use crate::{ActorContext, ManualClock};
use agent_domain::{FactBody, ProviderEvent, Role, State, Timestamp, fold};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Harness {
    _dir: tempfile::TempDir,
    registry: Arc<ActorRegistry>,
}
fn harness() -> Harness {
    let (dir, store) = temp_store();
    let mut context = ActorContext::new(store);
    context.clock = Arc::new(ManualClock::new(&Timestamp::parse(NOW).unwrap()));
    Harness {
        _dir: dir,
        registry: ActorRegistry::new(context),
    }
}
impl Harness {
    fn store(&self) -> &Store {
        &self.registry.context().store
    }
    fn importer(&self, source: impl SessionSource + 'static) -> Importer {
        Importer::new(self.registry.clone(), Arc::new(source))
    }
    fn facts(&self, id: &str) -> Vec<FactBody> {
        self.store()
            .facts_after(Some(&thread(id)), 0)
            .unwrap()
            .into_iter()
            .map(|f| f.fact.body)
            .collect()
    }
    fn state(&self, id: &str) -> State {
        let facts: Vec<_> = self
            .store()
            .facts_after(Some(&thread(id)), 0)
            .unwrap()
            .into_iter()
            .map(|f| f.fact)
            .collect();
        fold(&State::default(), &facts).unwrap()
    }
    fn recorded(&self) -> usize {
        self.store()
            .read(|c| {
                Ok(
                    c.query_row("SELECT COUNT(*) FROM imported_sources", [], |row| {
                        row.get::<_, i64>(0)
                    })? as usize,
                )
            })
            .unwrap()
    }
}

/// Returns the same outcomes on every call, ignoring completed sources.
struct FixedSource(Vec<RecentThread>);
impl SessionSource for FixedSource {
    fn recent_threads(
        &self,
        _: &Path,
        _: &[ImportSource],
    ) -> Box<dyn Iterator<Item = RecentThread> + Send> {
        Box::new(self.0.clone().into_iter())
    }
}

struct Projects {
    projects: Vec<ImportProject>,
    calls: AtomicUsize,
}
impl Projects {
    fn new(projects: Vec<ImportProject>) -> Self {
        Self {
            projects,
            calls: AtomicUsize::new(0),
        }
    }
}
impl ProjectRoots for Projects {
    fn projects(&self) -> Vec<ImportProject> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.projects.clone()
    }
}

fn at(value: &str) -> Timestamp {
    Timestamp::parse(value).unwrap()
}

fn importable(source: Driver, instance: &str, session: &str, path: &str) -> RecentThread {
    RecentThread::Importable {
        source: ImportSource {
            provider: source,
            instance: instance.into(),
            session: session.into(),
            path: path.into(),
            size: 100,
            mtime_ms: Some(2),
            device: 3,
            inode: Some(4),
            birthtime_ms: Some(1),
        },
        thread: SessionThread {
            source,
            instance: instance.into(),
            session: session.into(),
            title: "Imported thread".into(),
            model: Some("gpt-5.4".into()),
            created_at: at("2026-09-01T10:00:00.000Z"),
            updated_at: at("2026-09-01T10:01:00.000Z"),
            messages: vec![
                SessionMessage {
                    role: Role::User,
                    text: "Fix it".into(),
                    created_at: at("2026-09-01T10:00:00.000Z"),
                },
                SessionMessage {
                    role: Role::Assistant,
                    text: "Fixed".into(),
                    created_at: at("2026-09-01T10:01:00.000Z"),
                },
            ],
        },
    }
}

fn project(id: &str, root: impl Into<PathBuf>) -> ImportProject {
    ImportProject {
        id: id.into(),
        root: root.into(),
    }
}

const IMPORTED: ImportCounts = ImportCounts {
    imported: 1,
    skipped: 0,
};

// The fake scanner yields the same transcript on every run; recorded sources
// are rows, so the second identical record is one row.
#[tokio::test]
async fn imports_messages_once_and_preserves_the_provider_native_resume_binding() {
    let h = harness();
    let id = "import:codex:native-codex-thread";
    let importer = h.importer(FixedSource(vec![importable(
        Driver::Codex,
        "codex",
        "native-codex-thread",
        "/tmp/native-codex-thread.jsonl",
    )]));
    let project = project("agent-session-import-project", "/workspace/project");

    assert_eq!(importer.import_project(&project).await.unwrap(), IMPORTED);
    let written = h.facts(id);
    assert_eq!(importer.import_project(&project).await.unwrap(), IMPORTED);

    assert_eq!(h.facts(id), written);
    assert!(matches!(
        written.first(),
        Some(FactBody::ThreadCreated { .. })
    ));
    assert!(matches!(
        written.last(),
        Some(FactBody::NativeSessionBound { instance, native_thread, .. })
            if instance == "codex" && native_thread == "native-codex-thread"
    ));
    let state = h.state(id);
    let created = state.thread.as_ref().unwrap();
    assert_eq!(created.id.as_str(), id);
    assert!(created.imported);
    assert_eq!(created.selection.model, "gpt-5.4");
    assert_eq!(state.native_sessions["codex"], "native-codex-thread");
    assert_eq!(
        state
            .messages
            .iter()
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>(),
        ["Fix it", "Fixed"]
    );
    assert_eq!(h.recorded(), 1);
}

#[tokio::test]
async fn imported_threads_run_in_the_project_checkout_without_a_worktree() {
    let h = harness();
    let importer = h.importer(FixedSource(vec![importable(
        Driver::Codex,
        "codex",
        "session",
        "/tmp/session.jsonl",
    )]));
    importer
        .import_project(&project("project", "/workspace/project"))
        .await
        .unwrap();

    let workspace = h.state("import:codex:session").thread.unwrap().workspace;
    assert_eq!(
        workspace,
        Some(Workspace {
            cwd: "/workspace/project".into(),
            worktree_path: None,
            branch: None,
        })
    );
}

#[tokio::test]
async fn skips_claude_sessions_that_cannot_be_resumed() {
    let h = harness();
    let uuid = "0f8b7c1e-2d3a-4b5c-9d6e-7f8091a2b3c4";
    let importer = h.importer(FixedSource(vec![
        importable(Driver::Claude, "claude", "not-a-session", "/tmp/a.jsonl"),
        importable(Driver::Claude, "claude", uuid, "/tmp/b.jsonl"),
    ]));

    assert_eq!(
        importer.import_project(&project("p", "/w")).await.unwrap(),
        ImportCounts {
            imported: 1,
            skipped: 1
        }
    );
    assert!(h.facts("import:claude:not-a-session").is_empty());
    assert!(!h.facts(&format!("import:claude:{uuid}")).is_empty());
}

#[tokio::test]
async fn skips_an_imported_thread_that_belongs_to_another_project() {
    let h = harness();
    let importer = h.importer(FixedSource(vec![importable(
        Driver::Codex,
        "codex",
        "session",
        "/tmp/session.jsonl",
    )]));
    importer
        .import_project(&project("first", "/w"))
        .await
        .unwrap();

    assert_eq!(
        importer
            .import_project(&project("second", "/w"))
            .await
            .unwrap(),
        ImportCounts {
            imported: 0,
            skipped: 1
        }
    );
    assert_eq!(
        h.state("import:codex:session").thread.unwrap().project,
        "first"
    );
}

#[tokio::test]
async fn records_duplicate_copies_only_for_an_imported_session() {
    let h = harness();
    let duplicate = |path: &str| {
        let RecentThread::Importable { source, .. } =
            importable(Driver::Codex, "codex", "session", path)
        else {
            unreachable!()
        };
        RecentThread::Duplicate { source }
    };
    let importer = h.importer(FixedSource(vec![
        duplicate("/tmp/orphan.jsonl"),
        importable(Driver::Codex, "codex", "session", "/tmp/a.jsonl"),
        duplicate("/tmp/b.jsonl"),
    ]));

    assert_eq!(
        importer.import_project(&project("p", "/w")).await.unwrap(),
        IMPORTED
    );
    assert_eq!(h.recorded(), 2);
}

#[tokio::test]
async fn rejects_unknown_projects_and_moved_roots() {
    let h = harness();
    let importer = h.importer(FixedSource(vec![]));
    let projects = Projects::new(vec![project("p", "/w/")]);

    assert!(matches!(
        importer.import(&projects, "missing", None).await,
        Err(ImportError::ProjectNotFound(id)) if id == "missing"
    ));
    assert!(matches!(
        importer.import(&projects, "p", Some(Path::new("/elsewhere"))).await,
        Err(ImportError::ProjectChanged(id)) if id == "p"
    ));
    assert_eq!(
        importer
            .import(&projects, "p", Some(Path::new("/w")))
            .await
            .unwrap(),
        ImportCounts::default()
    );
}

#[tokio::test]
async fn an_import_command_cannot_create_another_thread() {
    let h = harness();
    let RecentThread::Importable {
        thread: session, ..
    } = importable(Driver::Codex, "codex", "session", "/tmp/a.jsonl")
    else {
        unreachable!()
    };
    let target = thread("import:codex:session");
    let committed = h
        .registry
        .dispatch(
            &thread("other"),
            command_id("import"),
            import_command(&project("p", "/w"), &target, session),
            CommandOrigin::Internal,
        )
        .await
        .unwrap();

    assert_eq!(
        committed.reply,
        Reply::Rejected {
            reason: "thread-mismatch".into()
        }
    );
}

/// Runs a Host thread whose provider reports `native` as its session.
async fn bind_host_session(h: &Harness, own: &ThreadId, native: &str) {
    let handle = h.registry.get_or_load(own).await.unwrap();
    handle
        .dispatch(command_id("create"), create(own), CommandOrigin::Client)
        .await
        .unwrap();
    let committed = handle
        .dispatch(command_id("send"), send("hello"), CommandOrigin::Client)
        .await
        .unwrap();
    let Reply::Run(run) = committed.reply else {
        panic!("{committed:?}")
    };
    let state = handle.view().await.unwrap().state;
    let attempt = state
        .runs
        .iter()
        .find(|r| r.id == run)
        .unwrap()
        .attempt
        .clone()
        .unwrap();
    handle
        .provider(
            attempt,
            ProviderEvent::SessionReady {
                native_thread: native.into(),
            },
        )
        .await
        .unwrap();
}

// The ownership check belongs to the commit: an import decided before the Host bound
// the session must still lose, and may be retried once the conflict is gone.
#[tokio::test]
async fn an_import_commit_rejects_a_session_bound_after_the_scan() {
    let h = harness();
    let RecentThread::Importable {
        thread: session, ..
    } = importable(Driver::Codex, "codex", "raced", "/tmp/raced.jsonl")
    else {
        unreachable!()
    };
    let target = thread("import:codex:raced");
    let import = import_command(&project("p", "/w"), &target, session);
    bind_host_session(&h, &thread("thread:own"), "raced").await;

    let committed = h
        .registry
        .dispatch(
            &target,
            command_id("import"),
            import.clone(),
            CommandOrigin::Internal,
        )
        .await
        .unwrap();

    assert_eq!(
        committed.reply,
        Reply::Rejected {
            reason: "native-session-owned".into()
        }
    );
    assert!(h.facts("import:codex:raced").is_empty());
    assert!(h.store().receipt(&command_id("import")).unwrap().is_none());
    let handle = h.registry.get_or_load(&target).await.unwrap();
    assert!(handle.view().await.unwrap().state.thread.is_none());
}

// The Host's own provider sessions write to the same homes; importing them would
// let two threads drive one native session.
#[tokio::test]
async fn skips_sessions_that_a_host_thread_already_owns() {
    let h = harness();
    let own = thread("thread:own");
    bind_host_session(&h, &own, "host-session").await;
    let handle = h.registry.get_or_load(&own).await.unwrap();
    assert_eq!(
        handle.view().await.unwrap().state.native_sessions["codex"],
        "host-session"
    );
    let importer = h.importer(FixedSource(vec![importable(
        Driver::Codex,
        "codex",
        "host-session",
        "/tmp/host.jsonl",
    )]));

    assert_eq!(
        importer.import_project(&project("p", "/w")).await.unwrap(),
        ImportCounts {
            imported: 0,
            skipped: 1
        }
    );
    assert!(h.facts("import:codex:host-session").is_empty());
}

fn scanner_importer(h: &Harness, setup: &Setup) -> Importer {
    Importer::new(h.registry.clone(), Arc::new(setup.scanner()))
}

// Imports go only to registered roots, matched by filesystem identity: a session in a
// subdirectory stays out, one recorded through a symlink comes in.
#[tokio::test]
async fn imports_only_sessions_whose_cwd_is_a_registered_project_root() {
    let h = harness();
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    let root = temps.dir("project-");
    let sub = root.join("packages/web");
    std::fs::create_dir_all(&sub).unwrap();
    let alias = temps.dir("links-").join("project-alias");
    std::os::unix::fs::symlink(&root, &alias).unwrap();
    let day = ["2026", "08", "24"];
    for (index, (session, cwd)) in [("root", &root), ("sub", &sub), ("alias", &alias)]
        .into_iter()
        .enumerate()
    {
        write_transcript(
            &rollout(&setup.codex, day, &format!("rollout-{session}.jsonl")),
            codex_session(session, cwd, "Work here"),
            ms(NOW) - index as i64 * 1_000,
        );
    }

    let counts = scanner_importer(&h, &setup)
        .import_project(&project("p", &root))
        .await
        .unwrap();
    assert_eq!(
        counts,
        ImportCounts {
            imported: 2,
            skipped: 0
        }
    );
    assert!(!h.facts("import:codex:root").is_empty());
    assert!(!h.facts("import:codex:alias").is_empty());
    assert!(h.facts("import:codex:sub").is_empty());
}

#[tokio::test]
async fn reimporting_a_project_does_not_read_imported_transcripts_again() {
    let h = harness();
    let mut temps = Temps::default();
    let mut setup = Setup::new(&mut temps);
    let root = temps.dir("project-");
    let transcript = rollout(&setup.codex, ["2026", "08", "24"], "rollout-once.jsonl");
    write_transcript(
        &transcript,
        codex_session("once", &root, "Import once"),
        ms(NOW),
    );
    let project = project("p", &root);
    assert_eq!(
        scanner_importer(&h, &setup)
            .import_project(&project)
            .await
            .unwrap(),
        IMPORTED
    );

    let opens = Arc::new(AtomicUsize::new(0));
    let counting = opens.clone();
    setup.fs = Arc::new(HookFs {
        open: Some(Box::new(move |path| {
            if path == transcript {
                counting.fetch_add(1, Ordering::SeqCst);
            }
            None
        })),
        ..HookFs::default()
    });
    assert_eq!(
        scanner_importer(&h, &setup)
            .import_project(&project)
            .await
            .unwrap(),
        IMPORTED
    );
    // Only the metadata read that finds the cwd; the transcript itself is not read.
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    assert_eq!(h.recorded(), 1);
}

#[tokio::test]
async fn the_first_run_imports_every_registered_project_once() {
    let h = harness();
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    let (first, second) = (temps.dir("first-"), temps.dir("second-"));
    let day = ["2026", "08", "24"];
    write_transcript(
        &rollout(&setup.codex, day, "rollout-a.jsonl"),
        codex_session("a", &first, "A"),
        ms(NOW),
    );
    write_transcript(
        &rollout(&setup.codex, day, "rollout-b.jsonl"),
        codex_session("b", &second, "B"),
        ms(NOW) - 1,
    );
    let projects = Arc::new(Projects::new(vec![
        project("first", &first),
        project("second", &second),
    ]));
    let importer = Arc::new(scanner_importer(&h, &setup));

    let counts = importer.first_run(projects.as_ref()).await.unwrap();
    assert_eq!(
        counts,
        Some(ImportCounts {
            imported: 2,
            skipped: 0
        })
    );
    assert_eq!(h.state("import:codex:b").thread.unwrap().project, "second");

    assert_eq!(importer.first_run(projects.as_ref()).await.unwrap(), None);
    assert_eq!(projects.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn claude_session_ids_must_be_uuids() {
    assert!(claude_session_id("0f8b7c1e-2d3a-4b5c-9d6e-7f8091a2b3c4"));
    assert!(claude_session_id("0F8B7C1E-2D3A-8B5C-BD6E-7F8091A2B3C4"));
    for invalid in [
        "replacement-session",
        "0f8b7c1e-2d3a-0b5c-9d6e-7f8091a2b3c4",
        "0f8b7c1e-2d3a-4b5c-cd6e-7f8091a2b3c4",
        "0f8b7c1e-2d3a-4b5c-9d6e-7f8091a2b3c",
        "0f8b7c1e2d3a4b5c9d6e7f8091a2b3c4",
    ] {
        assert!(!claude_session_id(invalid), "{invalid}");
    }
}

#[test]
fn the_native_owner_lookup_uses_its_index() {
    let h = harness();
    let plan = h
        .store()
        .read(|c| {
            let mut statement = c.prepare(&format!(
                "EXPLAIN QUERY PLAN {}",
                crate::store::NATIVE_OWNER_QUERY
            ))?;
            let rows = statement.query_map(rusqlite::params!["session", "thread"], |row| {
                row.get::<_, String>(3)
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?.join("\n"))
        })
        .unwrap();
    assert!(plan.contains("facts_native_session"), "{plan}");
}
