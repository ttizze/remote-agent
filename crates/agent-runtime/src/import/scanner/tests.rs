// The harness passes resolved instance homes, a stand-in user home and the
// Host's managed directories instead of server settings.
use super::*;
use crate::ManualClock;
use crate::import::fs::{OsFs, TranscriptFile};
use crate::import::record::tests::record_limit_transcript;
use serde_json::{Value, json};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, UNIX_EPOCH};

pub(crate) const NOW: &str = "2026-08-24T12:00:00.000Z";
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

pub(crate) fn ms(value: &str) -> i64 {
    Timestamp::parse(value).unwrap().millis()
}

#[derive(Default)]
pub(crate) struct Temps(Vec<tempfile::TempDir>);
impl Temps {
    pub(crate) fn dir(&mut self, prefix: &str) -> PathBuf {
        let dir = tempfile::Builder::new().prefix(prefix).tempdir().unwrap();
        let path = dir.path().to_path_buf();
        self.0.push(dir);
        path
    }
}

pub(crate) fn write_transcript(path: &Path, contents: impl AsRef<[u8]>, mtime_ms: i64) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
    let mtime = UNIX_EPOCH + Duration::from_millis(mtime_ms as u64);
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
}

pub(crate) fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Claude session line: the first record carries the real `cwd`.
fn claude_session_line(cwd: &Path) -> String {
    format!(
        "{}\n{}\n",
        json!({ "type": "user", "cwd": text(cwd), "sessionId": "s1" }),
        json!({ "type": "assistant" })
    )
}

/// Codex rollout line: session metadata is nested under `payload`.
fn codex_rollout_line(cwd: &Path) -> String {
    format!(
        "{}\n",
        json!({ "timestamp": "2026-01-01T00:00:00.000Z", "type": "session_meta",
                "payload": { "id": "r1", "cwd": text(cwd) } })
    )
}

fn lines(records: &[Value]) -> String {
    records
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn codex_session(id: &str, cwd: &Path, prompt: &str) -> String {
    lines(&[
        json!({ "type": "session_meta", "payload": { "id": id, "cwd": text(cwd) } }),
        json!({ "type": "event_msg", "payload": { "type": "user_message", "message": prompt } }),
    ])
}

pub(crate) fn rollout(home: &Path, day: [&str; 3], name: &str) -> PathBuf {
    home.join("sessions")
        .join(day[0])
        .join(day[1])
        .join(day[2])
        .join(name)
}

pub(crate) type DirHook = Box<dyn Fn(&Path) -> Option<Vec<String>> + Send + Sync>;
pub(crate) type StatHook = Box<dyn Fn(&Path) -> PathBuf + Send + Sync>;
pub(crate) type OpenHook =
    Box<dyn Fn(&Path) -> Option<io::Result<Box<dyn TranscriptFile>>> + Send + Sync>;

/// The real file system with substituted directory listings, stat targets and opens.
#[derive(Default)]
pub(crate) struct HookFs {
    pub(crate) read_dir: Option<DirHook>,
    pub(crate) stat: Option<StatHook>,
    pub(crate) open: Option<OpenHook>,
}
impl TranscriptFs for HookFs {
    fn read_dir(&self, dir: &Path) -> io::Result<Vec<String>> {
        match self.read_dir.as_ref().and_then(|hook| hook(dir)) {
            Some(names) => Ok(names),
            None => OsFs.read_dir(dir),
        }
    }
    fn stat(&self, path: &Path) -> io::Result<FileStat> {
        let target = self
            .stat
            .as_ref()
            .map_or_else(|| path.to_path_buf(), |hook| hook(path));
        OsFs.stat(&target)
    }
    fn real_path(&self, path: &Path) -> io::Result<PathBuf> {
        OsFs.real_path(path)
    }
    fn open(&self, path: &Path) -> io::Result<Box<dyn TranscriptFile>> {
        match self.open.as_ref().and_then(|hook| hook(path)) {
            Some(file) => file,
            None => OsFs.open(path),
        }
    }
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        OsFs.read_to_string(path)
    }
}

type ReadHook = Box<dyn FnMut(usize, Option<&[u8]>) + Send>;
struct Observed {
    inner: Box<dyn TranscriptFile>,
    on_read: ReadHook,
}
impl TranscriptFile for Observed {
    fn read(&mut self, max: usize) -> io::Result<Option<Vec<u8>>> {
        let chunk = self.inner.read(max)?;
        (self.on_read)(max, chunk.as_deref());
        Ok(chunk)
    }
    fn stat(&self) -> io::Result<FileStat> {
        self.inner.stat()
    }
}

pub(crate) struct Setup {
    pub(crate) claude: PathBuf,
    pub(crate) codex: PathBuf,
    config_base: PathBuf,
    home: PathBuf,
    instances: Vec<(Driver, &'static str, PathBuf)>,
    pub(crate) fs: Arc<dyn TranscriptFs>,
}
impl Setup {
    pub(crate) fn new(temps: &mut Temps) -> Self {
        Self {
            claude: temps.dir("claude-home-"),
            codex: temps.dir("codex-home-"),
            config_base: temps.dir("scanner-config-"),
            home: temps.dir("user-home-"),
            instances: Vec::new(),
            fs: Arc::new(OsFs),
        }
    }

    /// Configured instances in order, then the built-in instances they do not replace.
    fn homes(&self) -> Vec<ImportHome> {
        let mut homes: Vec<ImportHome> = self
            .instances
            .iter()
            .map(|(driver, instance, path)| ImportHome {
                driver: *driver,
                instance: (*instance).into(),
                path: path.clone(),
            })
            .collect();
        for (driver, path) in [(Driver::Claude, &self.claude), (Driver::Codex, &self.codex)] {
            let instance = default_instance(driver);
            if !self
                .instances
                .iter()
                .any(|(d, id, _)| *d == driver && *id == instance)
            {
                homes.push(ImportHome {
                    driver,
                    instance: instance.into(),
                    path: path.clone(),
                });
            }
        }
        homes
    }

    pub(crate) fn scanner(&self) -> Scanner {
        Scanner::new(
            ScanConfig {
                homes: self.homes(),
                home_dir: self.home.clone(),
                temp_dir: std::env::temp_dir(),
                managed_dirs: vec![self.config_base.clone(), self.config_base.join("worktrees")],
            },
            self.fs.clone(),
            Arc::new(ManualClock::new(&Timestamp::parse(NOW).unwrap())),
        )
    }

    fn scan(&self, imported: &[&Path]) -> ScanResult {
        self.scanner().scan(&projects(imported))
    }

    fn recent(&self, root: &Path) -> Vec<RecentThread> {
        self.scanner().recent_threads(root, &[]).collect()
    }

    fn threads(&self, root: &Path) -> Vec<SessionThread> {
        importable(self.recent(root))
    }
}

fn projects(roots: &[&Path]) -> Vec<ImportProject> {
    roots
        .iter()
        .map(|root| ImportProject {
            id: "project-1".into(),
            root: root.to_path_buf(),
        })
        .collect()
}

fn importable(outcomes: Vec<RecentThread>) -> Vec<SessionThread> {
    outcomes
        .into_iter()
        .filter_map(|outcome| match outcome {
            RecentThread::Importable { thread, .. } => Some(thread),
            _ => None,
        })
        .collect()
}

fn tags(outcomes: &[RecentThread]) -> Vec<&'static str> {
    outcomes
        .iter()
        .map(|outcome| match outcome {
            RecentThread::Importable { .. } => "Importable",
            RecentThread::AlreadyImported { .. } => "AlreadyImported",
            RecentThread::Duplicate { .. } => "Duplicate",
            RecentThread::Skipped => "Skipped",
        })
        .collect()
}

fn paths(result: &ScanResult) -> Vec<PathBuf> {
    result.candidates.iter().map(|c| c.path.clone()).collect()
}

fn basename(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

fn candidate(
    path: &Path,
    sources: &[Driver],
    thread_count: usize,
    last_active_at: &str,
) -> ProjectCandidate {
    ProjectCandidate {
        path: path.to_path_buf(),
        title: basename(path),
        project: None,
        sources: sources.to_vec(),
        thread_count,
        last_active_at: Some(Timestamp::parse(last_active_at).unwrap()),
        already_imported: false,
        git: None,
    }
}

fn counter() -> Arc<AtomicUsize> {
    Arc::new(AtomicUsize::new(0))
}

#[test]
fn reads_claude_project_cwds_from_transcripts_newest_first() {
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    let older = temps.dir("workspace-older-");
    let newer = temps.dir("workspace-newer-");
    // Slugs are intentionally lossy; the scanner must not decode them.
    let project = setup.claude.join("projects");
    write_transcript(
        &project.join("-slug-older/a.jsonl"),
        claude_session_line(&older),
        ms("2026-01-01T00:00:00.000Z"),
    );
    write_transcript(
        &project.join("-slug-older/b.jsonl"),
        claude_session_line(&older),
        ms("2026-01-02T00:00:00.000Z"),
    );
    write_transcript(
        &project.join("-slug-newer/c.jsonl"),
        claude_session_line(&newer),
        ms("2026-03-01T00:00:00.000Z"),
    );

    assert_eq!(
        setup.scan(&[]).candidates,
        [
            candidate(&newer, &[Driver::Claude], 1, "2026-03-01T00:00:00.000Z"),
            candidate(&older, &[Driver::Claude], 2, "2026-01-02T00:00:00.000Z"),
        ]
    );
}

#[test]
fn groups_codex_rollouts_by_cwd_across_date_directories() {
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    let workspace = temps.dir("workspace-");
    let other = temps.dir("workspace-other-");
    let home = &setup.codex;
    write_transcript(
        &rollout(
            home,
            ["2026", "01", "05"],
            "rollout-2026-01-05T10-00-00-aaa.jsonl",
        ),
        codex_rollout_line(&workspace),
        ms("2026-01-05T10:00:00.000Z"),
    );
    write_transcript(
        &rollout(
            home,
            ["2026", "02", "09"],
            "rollout-2026-02-09T10-00-00-bbb.jsonl",
        ),
        codex_rollout_line(&workspace),
        ms("2026-02-09T10:00:00.000Z"),
    );
    write_transcript(
        &rollout(
            home,
            ["2026", "02", "09"],
            "rollout-2026-02-09T11-00-00-ccc.jsonl",
        ),
        codex_rollout_line(&other),
        ms("2026-02-09T11:00:00.000Z"),
    );

    assert_eq!(
        setup.scan(&[]).candidates,
        [
            candidate(&other, &[Driver::Codex], 1, "2026-02-09T11:00:00.000Z"),
            candidate(&workspace, &[Driver::Codex], 2, "2026-02-09T10:00:00.000Z"),
        ]
    );
}

#[test]
fn does_not_open_a_non_file_transcript() {
    for source in [Driver::Claude, Driver::Codex] {
        let mut temps = Temps::default();
        let mut setup = Setup::new(&mut temps);
        let transcript = match source {
            Driver::Claude => setup.claude.join("projects/-slug/session.jsonl"),
            Driver::Codex => rollout(&setup.codex, ["2026", "08", "24"], "rollout-session.jsonl"),
        };
        std::fs::create_dir_all(&transcript).unwrap();
        let opens = counter();
        let (target, count) = (transcript.clone(), opens.clone());
        setup.fs = Arc::new(HookFs {
            open: Some(Box::new(move |path| {
                if path == target {
                    count.fetch_add(1, Ordering::SeqCst);
                }
                None
            })),
            ..HookFs::default()
        });

        assert_eq!(setup.scan(&[]).candidates, []);
        assert_eq!(opens.load(Ordering::SeqCst), 0, "{source:?}");
    }
}

#[test]
fn stops_directory_reads_at_the_discovery_operation_budget() {
    for source in [Driver::Claude, Driver::Codex] {
        let mut temps = Temps::default();
        let mut setup = Setup::new(&mut temps);
        let root = match source {
            Driver::Claude => setup.claude.join("projects"),
            Driver::Codex => setup.codex.join("sessions"),
        };
        let reads = counter();
        let count = reads.clone();
        setup.fs = Arc::new(HookFs {
            read_dir: Some(Box::new(move |dir| {
                if dir == root {
                    count.fetch_add(1, Ordering::SeqCst);
                    return Some((0..20_001).map(|i| format!("empty-{i:05}")).collect());
                }
                if dir.parent() == Some(root.as_path()) {
                    count.fetch_add(1, Ordering::SeqCst);
                    return Some(vec![]);
                }
                None
            })),
            ..HookFs::default()
        });

        assert_eq!(setup.scan(&[]).candidates, []);
        assert_eq!(reads.load(Ordering::SeqCst), 20_000, "{source:?}");
    }
}

#[test]
fn merges_the_same_cwd_seen_by_both_agents_and_flags_imported_projects() {
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    let workspace = temps.dir("workspace-");
    write_transcript(
        &setup.claude.join("projects/-slug/a.jsonl"),
        claude_session_line(&workspace),
        ms("2026-01-01T00:00:00.000Z"),
    );
    write_transcript(
        &rollout(
            &setup.codex,
            ["2026", "04", "01"],
            "rollout-2026-04-01T09-00-00-aaa.jsonl",
        ),
        codex_rollout_line(&workspace),
        ms("2026-04-01T09:00:00.000Z"),
    );

    assert_eq!(
        setup.scan(&[&workspace]).candidates,
        [ProjectCandidate {
            project: Some("project-1".into()),
            already_imported: true,
            ..candidate(
                &workspace,
                &[Driver::Claude, Driver::Codex],
                2,
                "2026-04-01T09:00:00.000Z"
            )
        }]
    );
}

#[test]
fn returns_the_imported_project_id_through_a_realpath_alias() {
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    let workspace = temps.dir("workspace-");
    let alias = temps.dir("scanner-links-").join("workspace-alias");
    std::os::unix::fs::symlink(&workspace, &alias).unwrap();
    write_transcript(
        &setup.claude.join("projects/-slug/a.jsonl"),
        claude_session_line(&alias),
        ms("2026-01-01T00:00:00.000Z"),
    );

    let first = setup.scan(&[&workspace]).candidates.remove(0);
    assert_eq!(first.path, workspace);
    assert_eq!(first.project.as_deref(), Some("project-1"));
    assert!(first.already_imported);
    assert_eq!(first.git, None);
}

#[test]
fn matches_a_persisted_project_alias_to_a_transcript_realpath() {
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    let workspace = temps.dir("workspace-");
    let alias = temps.dir("scanner-links-").join("workspace-alias");
    std::os::unix::fs::symlink(&workspace, &alias).unwrap();
    write_transcript(
        &setup.claude.join("projects/-slug/a.jsonl"),
        claude_session_line(&workspace),
        ms("2026-01-01T00:00:00.000Z"),
    );

    let first = setup.scan(&[&alias]).candidates.remove(0);
    assert_eq!(first.path, alias);
    assert_eq!(first.project.as_deref(), Some("project-1"));
    assert!(first.already_imported);
    assert_eq!(first.git, None);
}

#[test]
fn merges_case_aliases_and_preserves_the_persisted_project_path() {
    let mut temps = Temps::default();
    let mut setup = Setup::new(&mut temps);
    let workspace = temps.dir("workspace-");
    let alias = workspace.with_file_name(basename(&workspace).to_uppercase());
    write_transcript(
        &setup.claude.join("projects/-slug/a.jsonl"),
        claude_session_line(&alias),
        ms("2026-01-01T00:00:00.000Z"),
    );
    write_transcript(
        &rollout(&setup.codex, ["2026", "01", "02"], "rollout-b.jsonl"),
        codex_rollout_line(&workspace),
        ms("2026-01-02T00:00:00.000Z"),
    );
    let (from, to) = (alias.clone(), workspace.clone());
    setup.fs = Arc::new(HookFs {
        stat: Some(Box::new(move |path| {
            if path == from {
                to.clone()
            } else {
                path.to_path_buf()
            }
        })),
        ..HookFs::default()
    });

    assert_eq!(
        setup.scan(&[&workspace]).candidates,
        [ProjectCandidate {
            project: Some("project-1".into()),
            already_imported: true,
            ..candidate(
                &workspace,
                &[Driver::Claude, Driver::Codex],
                2,
                "2026-01-02T00:00:00.000Z"
            )
        }]
    );
}

#[test]
fn keeps_case_variants_distinct_when_the_filesystem_identities_differ() {
    let mut temps = Temps::default();
    let mut setup = Setup::new(&mut temps);
    let backing_upper = temps.dir("backing-upper-");
    let backing_lower = temps.dir("backing-lower-");
    let parent = temps.dir("case-aliases-");
    let (upper, lower) = (parent.join("Repo"), parent.join("repo"));
    write_transcript(
        &setup.claude.join("projects/-upper/a.jsonl"),
        claude_session_line(&upper),
        ms("2026-01-02T00:00:00.000Z"),
    );
    write_transcript(
        &setup.claude.join("projects/-lower/b.jsonl"),
        claude_session_line(&lower),
        ms("2026-01-01T00:00:00.000Z"),
    );
    let (u, l) = (upper.clone(), lower.clone());
    setup.fs = Arc::new(HookFs {
        stat: Some(Box::new(move |path| {
            if path == u {
                backing_upper.clone()
            } else if path == l {
                backing_lower.clone()
            } else {
                path.to_path_buf()
            }
        })),
        ..HookFs::default()
    });

    assert_eq!(paths(&setup.scan(&[])), [upper, lower]);
}

#[test]
fn uses_explicit_provider_instance_homes_instead_of_overridden_legacy_homes() {
    let mut temps = Temps::default();
    let mut setup = Setup::new(&mut temps);
    let claude_instance = temps.dir("claude-instance-");
    let codex_instance = temps.dir("codex-instance-");
    let legacy_workspace = temps.dir("workspace-legacy-");
    let claude_workspace = temps.dir("workspace-claude-");
    let codex_workspace = temps.dir("workspace-codex-");
    write_transcript(
        &setup.claude.join("projects/-legacy/session.jsonl"),
        claude_session_line(&legacy_workspace),
        ms("2026-01-01T00:00:00.000Z"),
    );
    write_transcript(
        &claude_instance.join("projects/-actual/session.jsonl"),
        claude_session_line(&claude_workspace),
        ms("2026-02-01T00:00:00.000Z"),
    );
    write_transcript(
        &rollout(
            &codex_instance,
            ["2026", "03", "01"],
            "rollout-instance.jsonl",
        ),
        codex_rollout_line(&codex_workspace),
        ms("2026-03-01T00:00:00.000Z"),
    );
    setup.instances = vec![
        (Driver::Claude, "claude", claude_instance),
        (Driver::Codex, "codex", codex_instance),
    ];

    assert_eq!(paths(&setup.scan(&[])), [codex_workspace, claude_workspace]);
}

#[test]
fn scans_each_distinct_home_across_multiple_instances_once() {
    let mut temps = Temps::default();
    let mut setup = Setup::new(&mut temps);
    let other_home = temps.dir("codex-other-");
    let workspace = temps.dir("workspace-");
    let other = temps.dir("workspace-other-");
    for (home, cwd) in [(&setup.codex, &workspace), (&other_home, &other)] {
        write_transcript(
            &rollout(home, ["2026", "01", "01"], "rollout-session.jsonl"),
            codex_rollout_line(cwd),
            ms("2026-01-01T00:00:00.000Z"),
        );
    }
    setup.instances = vec![
        (Driver::Codex, "codex-personal", setup.codex.clone()),
        (Driver::Codex, "codex-work", other_home),
    ];

    let result = setup.scan(&[]);
    assert_eq!(result.candidates.len(), 2);
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|c| c.thread_count)
            .collect::<Vec<_>>(),
        [1, 1]
    );
    let mut found = paths(&result);
    found.sort();
    let mut expected = vec![workspace, other];
    expected.sort();
    assert_eq!(found, expected);
}

#[test]
fn ignores_relative_working_directories_from_malformed_transcripts() {
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    let workspace = temps.dir("workspace-");
    let relative = workspace.strip_prefix("/").unwrap();
    write_transcript(
        &setup.claude.join("projects/-relative/session.jsonl"),
        claude_session_line(relative),
        ms("2026-01-01T00:00:00.000Z"),
    );

    assert_eq!(setup.scan(&[]).candidates, []);
}

#[test]
fn drops_candidates_whose_directory_no_longer_exists() {
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    write_transcript(
        &setup.claude.join("projects/-slug/a.jsonl"),
        claude_session_line(&setup.claude.join("does-not-exist")),
        ms("2026-01-01T00:00:00.000Z"),
    );

    assert_eq!(setup.scan(&[]).candidates, []);
}

#[test]
fn excludes_the_home_directory_temporary_root_and_data_directory() {
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    let workspace = temps.dir("workspace-");
    for (index, cwd) in [
        setup.home.clone(),
        std::env::temp_dir(),
        setup.config_base.clone(),
        workspace.clone(),
    ]
    .iter()
    .enumerate()
    {
        write_transcript(
            &setup
                .claude
                .join(format!("projects/-slug-{index}/session.jsonl")),
            claude_session_line(cwd),
            ms("2026-01-01T00:00:00.000Z") + index as i64,
        );
    }

    assert_eq!(paths(&setup.scan(&[])), [workspace]);
}

#[test]
fn excludes_managed_worktree_sandboxes() {
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    let worktree = setup.claude.join(".worktree/widget/wt-1");
    std::fs::create_dir_all(&worktree).unwrap();
    write_transcript(
        &setup.claude.join("projects/-slug/a.jsonl"),
        claude_session_line(&worktree),
        ms("2026-01-01T00:00:00.000Z"),
    );

    assert_eq!(setup.scan(&[]).candidates, []);
}

#[test]
fn excludes_codex_scratch_directories_and_downloads() {
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    let scratch = setup
        .home
        .join("Documents/Codex/run/2026-09-01/some-conversation");
    let downloads = setup.home.join("Downloads/run");
    let keep = temps.dir("workspace-keep-");
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::create_dir_all(&downloads).unwrap();
    for (index, cwd) in [&scratch, &downloads, &keep].iter().enumerate() {
        write_transcript(
            &rollout(
                &setup.codex,
                ["2026", "09", "01"],
                &format!("rollout-{index}.jsonl"),
            ),
            codex_rollout_line(cwd),
            ms("2026-09-01T00:00:00.000Z"),
        );
    }

    assert_eq!(paths(&setup.scan(&[])), [keep]);
}

#[test]
fn skips_linked_git_worktrees_and_reports_the_origin_of_real_checkouts() {
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    let repo = temps.dir("workspace-repo-");
    let worktree = temps.dir("workspace-worktree-");
    let plain = temps.dir("workspace-plain-");
    let no_remote = temps.dir("workspace-noremote-");
    let submodule = temps.dir("workspace-submodule-");
    std::fs::create_dir(repo.join(".git")).unwrap();
    std::fs::write(
        repo.join(".git/config"),
        "[core]\n\tbare = false\n[remote \"origin\"]\n\turl = git@github.com:pingdotgg/widget.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n",
    )
    .unwrap();
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", text(&repo.join(".git/worktrees/wt"))),
    )
    .unwrap();
    std::fs::create_dir(no_remote.join(".git")).unwrap();
    std::fs::write(no_remote.join(".git/config"), "[core]\n").unwrap();
    // Submodules also use a gitdir pointer, but into `modules/`, not `worktrees/`.
    let submodule_git = repo.join(".git/modules/vendor");
    std::fs::create_dir_all(&submodule_git).unwrap();
    std::fs::write(
        submodule_git.join("config"),
        "[remote \"origin\"]\n\turl = ssh://github.com/pingdotgg/vendor.git\n",
    )
    .unwrap();
    std::fs::write(
        submodule.join(".git"),
        format!("gitdir: {}\n", text(&submodule_git)),
    )
    .unwrap();
    for (index, cwd) in [&repo, &worktree, &plain, &no_remote, &submodule]
        .iter()
        .enumerate()
    {
        write_transcript(
            &setup.claude.join(format!("projects/-slug-{index}/a.jsonl")),
            claude_session_line(cwd),
            ms(&format!("2026-01-0{}T00:00:00.000Z", index + 1)),
        );
    }

    let git = |remote_key: Option<&str>, repository: Option<&str>| {
        Some(ProjectGit {
            remote_key: remote_key.map(Into::into),
            repository: repository.map(Into::into),
        })
    };
    assert_eq!(
        setup
            .scan(&[])
            .candidates
            .into_iter()
            .map(|c| (c.path, c.git))
            .collect::<Vec<_>>(),
        [
            (
                submodule,
                git(
                    Some("github.com/pingdotgg/vendor"),
                    Some("pingdotgg/vendor")
                )
            ),
            (no_remote, git(None, None)),
            (plain, None),
            (
                repo,
                git(
                    Some("github.com/pingdotgg/widget"),
                    Some("pingdotgg/widget")
                )
            ),
        ]
    );
}

#[test]
fn excludes_sandboxes_under_the_configured_worktrees_dir() {
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    let worktree = setup.config_base.join("worktrees/widget/wt-2");
    std::fs::create_dir_all(&worktree).unwrap();
    write_transcript(
        &setup.claude.join("projects/-slug/a.jsonl"),
        claude_session_line(&worktree),
        ms("2026-01-01T00:00:00.000Z"),
    );

    assert_eq!(setup.scan(&[]).candidates, []);
}

#[test]
fn excludes_sandboxes_reached_through_a_symlink_into_the_worktrees_dir() {
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    // The recorded cwd's own spelling looks harmless; only its realpath reveals the sandbox.
    let worktree = setup.config_base.join("worktrees/widget/wt-3");
    std::fs::create_dir_all(&worktree).unwrap();
    let link = temps.dir("scanner-links-").join("innocent-project");
    std::os::unix::fs::symlink(&worktree, &link).unwrap();
    write_transcript(
        &setup.claude.join("projects/-slug/a.jsonl"),
        claude_session_line(&link),
        ms("2026-01-01T00:00:00.000Z"),
    );

    assert_eq!(setup.scan(&[]).candidates, []);
}

#[test]
fn finds_the_cwd_on_a_later_line_when_the_first_records_carry_none() {
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    let workspace = temps.dir("workspace-");
    let contents = format!(
        "{{\"type\":\"file-history-snapshot\",\"messageId\":\"m1\"}}\n{{\"type\":\"queue-operation\",\"operation\":\"enqueue\"}}\n{}",
        claude_session_line(&workspace)
    );
    write_transcript(
        &setup.claude.join("projects/-slug/a.jsonl"),
        contents,
        ms("2026-01-01T00:00:00.000Z"),
    );

    assert_eq!(paths(&setup.scan(&[])), [workspace]);
}

#[test]
fn reads_a_complete_transcript_record_at_the_exact_chunk_boundary() {
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    let workspace = temps.dir("workspace-");
    let line = claude_session_line(&workspace);
    let record = line.split('\n').next().unwrap();
    let prefix = r#"{"padding":""#;
    let suffix = format!("\",{}", &record[1..]);
    let contents = format!(
        "{prefix}{}{suffix}",
        "x".repeat(32 * 1024 - prefix.len() - suffix.len())
    );
    write_transcript(
        &setup.claude.join("projects/-exact/session.jsonl"),
        &contents,
        ms("2026-01-01T00:00:00.000Z"),
    );

    assert_eq!(contents.len(), 32 * 1024);
    assert_eq!(paths(&setup.scan(&[])), [workspace]);
}

#[test]
fn finds_session_metadata_after_a_first_record_larger_than_one_chunk() {
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    let workspace = temps.dir("workspace-");
    let history = format!(
        "{{\"type\":\"file-history-snapshot\",\"data\":\"{}\"}}\n",
        "x".repeat(32 * 1024)
    );
    write_transcript(
        &setup.claude.join("projects/-large/session.jsonl"),
        format!("{history}{}", claude_session_line(&workspace)),
        ms("2026-01-01T00:00:00.000Z"),
    );

    assert_eq!(paths(&setup.scan(&[])), [workspace]);
}

#[test]
fn shares_metadata_bytes_across_homes_for_one_mib_files() {
    for count in [64, 65] {
        let mut temps = Temps::default();
        let mut setup = Setup::new(&mut temps);
        let second = temps.dir("metadata-second-");
        let first_workspace = temps.dir("metadata-first-project-");
        let second_workspace = temps.dir("metadata-second-project-");
        let directories = [setup.claude.join("projects/p"), second.join("projects/p")];
        let templates: Vec<PathBuf> = directories
            .iter()
            .map(|d| d.join("template.jsonl"))
            .collect();
        for (index, workspace) in [&first_workspace, &second_workspace].iter().enumerate() {
            let record = json!({ "cwd": text(workspace) }).to_string();
            write_transcript(
                &templates[index],
                format!("{}{record}", " ".repeat(1024 * 1024 - record.len())),
                ms("2026-01-01T00:00:00.000Z") - index as i64 * 1_000,
            );
        }
        let resolve_file = {
            let (directories, templates) = (directories.clone(), templates.clone());
            move |path: &Path| match directories
                .iter()
                .position(|d| Some(d.as_path()) == path.parent())
            {
                Some(index) => templates[index].clone(),
                None => path.to_path_buf(),
            }
        };
        let reserved = counter();
        let opens = counter();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let listed = directories.clone();
        let (stat_file, open_file) = (resolve_file.clone(), resolve_file);
        let (opened, reserving, requesting) = (opens.clone(), reserved.clone(), requests.clone());
        let observed = directories.clone();
        setup.fs = Arc::new(HookFs {
            read_dir: Some(Box::new(move |dir| {
                let index = listed.iter().position(|d| d == dir)?;
                let length = if index == 0 { 32 } else { count - 32 };
                Some(
                    (0..length)
                        .map(|item| format!("session-{item}.jsonl"))
                        .collect(),
                )
            })),
            stat: Some(Box::new(move |path| stat_file(path))),
            open: Some(Box::new(move |path| {
                if !observed.iter().any(|d| Some(d.as_path()) == path.parent()) {
                    return None;
                }
                opened.fetch_add(1, Ordering::SeqCst);
                let (reserving, requesting) = (reserving.clone(), requesting.clone());
                Some(OsFs.open(&open_file(path)).map(|inner| {
                    Box::new(Observed {
                        inner,
                        on_read: Box::new(move |size, _| {
                            reserving.fetch_add(size, Ordering::SeqCst);
                            requesting.lock().unwrap().push(size);
                        }),
                    }) as Box<dyn TranscriptFile>
                }))
            })),
        });
        setup.instances = vec![(Driver::Claude, "claude-work", second.clone())];

        let result = setup.scan(&[]);
        assert_eq!(paths(&result), [first_workspace, second_workspace]);
        assert_eq!(
            result
                .candidates
                .iter()
                .map(|c| c.thread_count)
                .collect::<Vec<_>>(),
            [32, 32]
        );
        assert_eq!(result.truncated, count == 65);
        assert_eq!(opens.load(Ordering::SeqCst), 64);
        assert_eq!(reserved.load(Ordering::SeqCst), 64 * 1024 * 1024);
        let requests = requests.lock().unwrap();
        assert_eq!(requests[0], 8 * 1024);
        assert_eq!(requests.iter().max(), Some(&(8 * 1024)));
    }
}

struct ByteAtATime {
    bytes: Vec<u8>,
    offset: usize,
    template: PathBuf,
    operations: Arc<AtomicUsize>,
}
impl TranscriptFile for ByteAtATime {
    fn read(&mut self, _: usize) -> io::Result<Option<Vec<u8>>> {
        self.operations.fetch_add(1, Ordering::SeqCst);
        if self.offset == self.bytes.len() {
            return Ok(None);
        }
        self.offset += 1;
        Ok(Some(vec![self.bytes[self.offset - 1]]))
    }
    fn stat(&self) -> io::Result<FileStat> {
        OsFs.stat(&self.template)
    }
}

#[test]
fn bounds_metadata_open_and_read_calls_for_short_read_files() {
    for count in [50, 51] {
        let mut temps = Temps::default();
        let mut setup = Setup::new(&mut temps);
        let workspace = temps.dir("short-metadata-project-");
        let directory = setup.claude.join("projects/p");
        let template = directory.join("template.jsonl");
        let record = json!({ "cwd": text(&workspace) }).to_string();
        let contents = format!("{}{record}", " ".repeat(399 - record.len()));
        write_transcript(&template, &contents, ms("2026-01-01T00:00:00.000Z"));
        let operations = counter();
        let (listed, statted, opened) = (directory.clone(), directory.clone(), directory.clone());
        let (stat_template, open_template) = (template.clone(), template.clone());
        let counting = operations.clone();
        setup.fs = Arc::new(HookFs {
            read_dir: Some(Box::new(move |dir| {
                (dir == listed).then(|| (0..count).map(|i| format!("session-{i}.jsonl")).collect())
            })),
            stat: Some(Box::new(move |path| {
                if path.parent() == Some(statted.as_path()) {
                    stat_template.clone()
                } else {
                    path.to_path_buf()
                }
            })),
            open: Some(Box::new(move |path| {
                if path.parent() != Some(opened.as_path()) {
                    return None;
                }
                counting.fetch_add(1, Ordering::SeqCst);
                Some(Ok(Box::new(ByteAtATime {
                    bytes: contents.clone().into_bytes(),
                    offset: 0,
                    template: open_template.clone(),
                    operations: counting.clone(),
                }) as Box<dyn TranscriptFile>))
            })),
        });

        let result = setup.scan(&[]);
        assert_eq!(operations.load(Ordering::SeqCst), 20_000);
        assert_eq!(result.candidates[0].thread_count, 50);
        assert_eq!(result.truncated, count == 51);
    }
}

#[test]
fn bounds_malformed_metadata_records_without_excluding_another_account() {
    let mut temps = Temps::default();
    let mut setup = Setup::new(&mut temps);
    let second = temps.dir("record-metadata-second-");
    let workspace = temps.dir("record-metadata-project-");
    let directory = setup.claude.join("projects/p");
    let template = directory.join("template.jsonl");
    write_transcript(
        &template,
        "x\n".repeat(1_001),
        ms("2026-01-02T00:00:00.000Z"),
    );
    write_transcript(
        &second.join("projects/p/session.jsonl"),
        json!({ "cwd": text(&workspace) }).to_string(),
        ms("2026-01-01T00:00:00.000Z"),
    );
    let malformed_opens = counter();
    let (listed, statted, opened) = (directory.clone(), directory.clone(), directory.clone());
    let (stat_template, open_template) = (template.clone(), template);
    let counting = malformed_opens.clone();
    setup.fs = Arc::new(HookFs {
        read_dir: Some(Box::new(move |dir| {
            (dir == listed).then(|| (0..102).map(|i| format!("session-{i}.jsonl")).collect())
        })),
        stat: Some(Box::new(move |path| {
            if path.parent() == Some(statted.as_path()) {
                stat_template.clone()
            } else {
                path.to_path_buf()
            }
        })),
        open: Some(Box::new(move |path| {
            if path.parent() != Some(opened.as_path()) {
                return None;
            }
            counting.fetch_add(1, Ordering::SeqCst);
            Some(OsFs.open(&open_template))
        })),
    });
    setup.instances = vec![(Driver::Claude, "claude-work", second)];

    let result = setup.scan(&[]);
    assert_eq!(paths(&result), [workspace]);
    assert_eq!(malformed_opens.load(Ordering::SeqCst), 100);
    assert!(result.truncated);
}

#[test]
fn reports_unfinished_directory_work_for_many_project_directories() {
    for count in [19_999, 20_000] {
        let mut temps = Temps::default();
        let mut setup = Setup::new(&mut temps);
        let projects_dir = setup.claude.join("projects");
        let reads = counter();
        let counting = reads.clone();
        setup.fs = Arc::new(HookFs {
            read_dir: Some(Box::new(move |dir| {
                if dir == projects_dir {
                    counting.fetch_add(1, Ordering::SeqCst);
                    return Some((0..count).map(|i| format!("project-{i}")).collect());
                }
                if dir.parent() == Some(projects_dir.as_path()) {
                    counting.fetch_add(1, Ordering::SeqCst);
                    return Some(vec![]);
                }
                None
            })),
            ..HookFs::default()
        });

        let result = setup.scan(&[]);
        assert_eq!(reads.load(Ordering::SeqCst), 20_000);
        assert_eq!(result.candidates, []);
        assert_eq!(result.truncated, count == 20_000);
    }
}

#[test]
fn skips_malformed_transcripts_without_failing_the_scan() {
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    let workspace = temps.dir("workspace-");
    write_transcript(
        &setup.claude.join("projects/-broken/a.jsonl"),
        "not json at all\n",
        ms("2026-05-01T00:00:00.000Z"),
    );
    // Valid JSON, but no cwd anywhere in the record.
    write_transcript(
        &setup.claude.join("projects/-no-cwd/a.jsonl"),
        "{\"type\":\"summary\"}\n",
        ms("2026-05-02T00:00:00.000Z"),
    );
    write_transcript(
        &setup.claude.join("projects/-good/a.jsonl"),
        claude_session_line(&workspace),
        ms("2026-05-03T00:00:00.000Z"),
    );

    assert_eq!(
        setup.scan(&[]).candidates,
        [candidate(
            &workspace,
            &[Driver::Claude],
            1,
            "2026-05-03T00:00:00.000Z"
        )]
    );
}

#[test]
fn returns_an_empty_result_when_neither_home_directory_exists() {
    let mut temps = Temps::default();
    let mut setup = Setup::new(&mut temps);
    let root = temps.dir("missing-homes-");
    setup.claude = root.join("no-claude");
    setup.codex = root.join("no-codex");

    let result = setup.scan(&[]);
    assert_eq!(result.candidates, []);
    assert!(result.scanned_at.as_str().starts_with("2026-08-24T"));
}

// recentThreads

#[test]
fn counts_terminal_newlines_correctly_with_record_overflow() {
    for overflow in [false, true] {
        let mut temps = Temps::default();
        let setup = Setup::new(&mut temps);
        let workspace = temps.dir("record-limit-project-");
        let day = ["2026", "08", "24"];
        write_transcript(
            &rollout(&setup.codex, day, "rollout-records.jsonl"),
            record_limit_transcript(&text(&workspace), overflow),
            ms(NOW),
        );
        write_transcript(
            &rollout(&setup.codex, day, "rollout-older.jsonl"),
            codex_session("older-session", &workspace, "Older prompt"),
            ms(NOW) - 1_000,
        );

        let outcomes = setup.recent(&workspace);
        assert_eq!(
            tags(&outcomes),
            if overflow {
                ["Skipped", "Importable"]
            } else {
                ["Importable", "Skipped"]
            }
        );
        let texts: Vec<String> = importable(outcomes)
            .into_iter()
            .flat_map(|thread| thread.messages.into_iter().map(|m| m.text))
            .collect();
        assert_eq!(
            texts,
            [if overflow {
                "Older prompt"
            } else {
                "First prompt"
            }]
        );
    }
}

#[test]
fn imports_recent_claude_and_codex_sessions_for_the_selected_project_only() {
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    let workspace = temps.dir("workspace-");
    let other = temps.dir("workspace-other-");
    let claude_transcript = |cwd: &Path, session: &str| {
        lines(&[
            json!({ "type": "user", "cwd": text(cwd), "sessionId": session,
                    "timestamp": "2026-08-23T12:00:00.000Z",
                    "message": { "role": "user", "content": "Fix the project" } }),
            json!({ "type": "assistant", "sessionId": session, "timestamp": "2026-08-23T12:01:00.000Z",
                    "message": { "role": "assistant", "content": [{ "type": "text", "text": "Done" }] } }),
        ]) + "\n"
    };
    let now = ms(NOW);
    let projects_dir = setup.claude.join("projects");
    write_transcript(
        &projects_dir.join("-selected/claude-recent.jsonl"),
        claude_transcript(&workspace, "claude-recent"),
        now - DAY_MS,
    );
    write_transcript(
        &projects_dir.join("-selected/claude-old.jsonl"),
        claude_transcript(&workspace, "claude-old"),
        now - 31 * DAY_MS,
    );
    write_transcript(
        &projects_dir.join("-other/claude-other.jsonl"),
        claude_transcript(&other, "claude-other"),
        now - DAY_MS,
    );
    write_transcript(
        &rollout(
            &setup.codex,
            ["2026", "08", "24"],
            "rollout-codex-recent.jsonl",
        ),
        lines(&[
            json!({ "type": "session_meta", "payload": { "id": "codex-recent", "cwd": text(&workspace) } }),
            json!({ "type": "event_msg", "timestamp": "2026-08-24T10:00:00.000Z",
                    "payload": { "type": "user_message", "message": "Review this code" } }),
            json!({ "type": "response_item", "timestamp": "2026-08-24T10:01:00.000Z",
                    "payload": { "type": "message", "role": "assistant",
                                 "content": [{ "type": "output_text", "text": "Looks good" }] } }),
        ]),
        now - 60 * 60 * 1000,
    );

    let threads = setup.threads(&workspace);
    assert_eq!(
        threads
            .iter()
            .map(|t| t.session.as_str())
            .collect::<Vec<_>>(),
        ["codex-recent", "claude-recent"]
    );
    assert_eq!(
        threads
            .iter()
            .map(|t| t
                .messages
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        [
            vec!["Review this code", "Looks good"],
            vec!["Fix the project", "Done"]
        ]
    );
}

#[test]
fn imports_history_recorded_with_a_case_alias() {
    let mut temps = Temps::default();
    let mut setup = Setup::new(&mut temps);
    let workspace = temps.dir("workspace-");
    let alias = workspace.with_file_name(basename(&workspace).to_uppercase());
    write_transcript(
        &setup.claude.join("projects/-alias/case-session.jsonl"),
        lines(&[
            json!({ "type": "user", "cwd": text(&alias), "sessionId": "case-session",
                    "timestamp": "2026-08-24T10:00:00.000Z",
                    "message": { "role": "user", "content": "Import case alias history" } }),
            json!({ "type": "assistant", "sessionId": "case-session", "timestamp": "2026-08-24T10:01:00.000Z",
                    "message": { "role": "assistant", "content": "Imported" } }),
        ]),
        ms(NOW),
    );
    let (from, to) = (alias.clone(), workspace.clone());
    setup.fs = Arc::new(HookFs {
        stat: Some(Box::new(move |path| {
            if path == from {
                to.clone()
            } else {
                path.to_path_buf()
            }
        })),
        ..HookFs::default()
    });

    assert_eq!(
        setup
            .threads(&workspace)
            .iter()
            .map(|t| t.session.as_str())
            .collect::<Vec<_>>(),
        ["case-session"]
    );
}

#[test]
fn keeps_the_provider_instance_that_owns_a_custom_session_home() {
    let mut temps = Temps::default();
    let mut setup = Setup::new(&mut temps);
    let custom = temps.dir("codex-custom-");
    let workspace = temps.dir("workspace-");
    write_transcript(
        &rollout(&custom, ["2026", "08", "24"], "rollout-custom.jsonl"),
        codex_session("custom-session", &workspace, "Use my work account"),
        ms(NOW),
    );
    setup.instances = vec![(Driver::Codex, "codex-work", custom)];

    assert_eq!(setup.threads(&workspace)[0].instance, "codex-work");
}

#[test]
fn suppresses_duplicate_session_copies_without_reporting_a_skipped_import() {
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    let workspace = temps.dir("workspace-");
    let contents = codex_session("copied-session", &workspace, "Import this session once");
    for (name, mtime) in [
        ("rollout-copy-a.jsonl", ms(NOW)),
        ("rollout-copy-b.jsonl", ms(NOW) - 1),
    ] {
        write_transcript(
            &rollout(&setup.codex, ["2026", "08", "24"], name),
            &contents,
            mtime,
        );
    }

    let outcomes = setup.recent(&workspace);
    assert_eq!(tags(&outcomes), ["Importable", "Duplicate"]);
    assert!(matches!(
        &outcomes[0],
        RecentThread::Importable { thread, .. } if thread.session == "copied-session"
    ));
}

#[test]
fn streams_large_transcripts_across_providers_without_hiding_projects() {
    let mut temps = Temps::default();
    let mut setup = Setup::new(&mut temps);
    let workspace = temps.dir("budget-workspace-");
    let mut transcripts = HashSet::new();
    for (index, source) in [
        Driver::Codex,
        Driver::Claude,
        Driver::Codex,
        Driver::Claude,
        Driver::Codex,
    ]
    .into_iter()
    .enumerate()
    {
        let session = format!("budget-session-{index}");
        let (path, contents) = match source {
            Driver::Codex => (
                rollout(
                    &setup.codex,
                    ["2026", "08", "24"],
                    &format!("rollout-{session}.jsonl"),
                ),
                codex_session(&session, &workspace, "Imported prompt"),
            ),
            Driver::Claude => (
                setup
                    .claude
                    .join(format!("projects/selected/{session}.jsonl")),
                json!({ "type": "user", "cwd": text(&workspace), "sessionId": session,
                        "message": { "content": "Imported prompt" } })
                .to_string(),
            ),
        };
        let mut padded = format!("{contents}\n").into_bytes();
        padded.resize(16 * 1024 * 1024, b' ');
        write_transcript(&path, padded, ms(NOW) - index as i64 * 1_000);
        transcripts.insert(path);
    }
    let opens: Arc<Mutex<HashMap<PathBuf, usize>>> = Arc::default();
    let full_read = counter();
    let (opened, reading) = (opens.clone(), full_read.clone());
    setup.fs = Arc::new(HookFs {
        open: Some(Box::new(move |path| {
            let count = {
                let mut opens = opened.lock().unwrap();
                let count = opens.entry(path.to_path_buf()).or_default();
                *count += 1;
                *count
            };
            if !transcripts.contains(path) || count == 1 {
                return None;
            }
            let reading = reading.clone();
            Some(OsFs.open(path).map(|inner| {
                Box::new(Observed {
                    inner,
                    on_read: Box::new(move |_, chunk| {
                        reading.fetch_add(chunk.map_or(0, <[u8]>::len), Ordering::SeqCst);
                    }),
                }) as Box<dyn TranscriptFile>
            }))
        })),
        ..HookFs::default()
    });

    let scanner = setup.scanner();
    assert_eq!(scanner.scan(&[]).candidates[0].thread_count, 5);
    let outcomes: Vec<RecentThread> = scanner.recent_threads(&workspace, &[]).collect();
    assert_eq!(tags(&outcomes), ["Importable"; 5]);
    assert_eq!(full_read.load(Ordering::SeqCst), 80 * 1024 * 1024);
}

#[test]
fn skips_excessive_records_without_blocking_an_older_valid_transcript() {
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    let workspace = temps.dir("record-budget-workspace-");
    for (session, padding, mtime) in [
        ("excessive", "\n".repeat(100_001), ms(NOW)),
        ("older", String::new(), ms(NOW) - 1_000),
    ] {
        write_transcript(
            &rollout(
                &setup.codex,
                ["2026", "08", "24"],
                &format!("rollout-{session}.jsonl"),
            ),
            codex_session(session, &workspace, "Imported prompt") + &padding,
            mtime,
        );
    }

    let outcomes = setup.recent(&workspace);
    assert_eq!(tags(&outcomes), ["Skipped", "Importable"]);
    assert!(
        matches!(&outcomes[1], RecentThread::Importable { thread, .. } if thread.session == "older")
    );
}

#[test]
fn rechecks_the_snapshot_cwd_after_replacement() {
    for source in [Driver::Claude, Driver::Codex] {
        for replacement in [
            "same root",
            "other root",
            "symlink alias",
            "other then same",
        ] {
            let mut temps = Temps::default();
            let fixture = temps.dir("replaced-cwd-");
            let workspace = fixture.join("original");
            let other = fixture.join("other");
            let alias = fixture.join("alias");
            let mut setup = Setup::new(&mut temps);
            setup.claude = fixture.join("claude");
            setup.codex = fixture.join("codex");
            std::fs::create_dir(&workspace).unwrap();
            std::fs::create_dir(&other).unwrap();
            if replacement == "symlink alias" {
                std::os::unix::fs::symlink(&workspace, &alias).unwrap();
            }
            let path = match source {
                Driver::Codex => {
                    rollout(&setup.codex, ["2026", "08", "24"], "rollout-replaced.jsonl")
                }
                Driver::Claude => setup.claude.join("projects/p/replaced.jsonl"),
            };
            let contents = |cwd: &Path, prompt: &str, later: Option<&Path>| {
                let mut records = match source {
                    Driver::Codex => vec![
                        json!({ "type": "session_meta", "payload": { "id": "replacement-session", "cwd": text(cwd) } }),
                        json!({ "type": "event_msg", "payload": { "type": "user_message", "message": prompt } }),
                    ],
                    Driver::Claude => vec![json!({ "type": "user", "cwd": text(cwd),
                        "sessionId": "replacement-session", "message": { "content": prompt } })],
                };
                records.extend(later.map(|cwd| json!({ "cwd": text(cwd) })));
                lines(&records)
            };
            write_transcript(
                &path,
                contents(&workspace, "Original prompt", None),
                ms(NOW),
            );

            let scanner = setup.scanner();
            assert_eq!(paths(&scanner.scan(&[])), std::slice::from_ref(&workspace));
            let replacement_cwd = match replacement {
                "symlink alias" => &alias,
                "same root" => &workspace,
                _ => &other,
            };
            std::fs::remove_file(&path).unwrap();
            write_transcript(
                &path,
                contents(
                    replacement_cwd,
                    "Replacement prompt",
                    (replacement == "other then same").then_some(workspace.as_path()),
                ),
                ms(NOW),
            );
            let outcomes: Vec<RecentThread> = scanner.recent_threads(&workspace, &[]).collect();
            if replacement == "same root" || replacement == "symlink alias" {
                assert_eq!(outcomes.len(), 1);
                let threads = importable(outcomes);
                assert_eq!(
                    threads[0].messages[0].text, "Replacement prompt",
                    "{source:?} {replacement}"
                );
            } else {
                assert_eq!(
                    outcomes,
                    [RecentThread::Skipped],
                    "{source:?} {replacement}"
                );
            }
        }
    }
}

#[test]
fn checks_file_identity_and_provider_before_skipping_completed_history() {
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    let workspace = temps.dir("completed-workspace-");
    let path = rollout(&setup.codex, ["2026", "08", "24"], "rollout-replaced.jsonl");
    write_transcript(
        &path,
        codex_session("original-session", &workspace, "Imported prompt"),
        ms(NOW),
    );

    let scanner = setup.scanner();
    let initial: Vec<RecentThread> = scanner.recent_threads(&workspace, &[]).collect();
    let RecentThread::Importable { source, .. } = &initial[0] else {
        panic!("expected an importable thread, got {initial:?}");
    };
    let completed: Vec<RecentThread> = scanner
        .recent_threads(&workspace, std::slice::from_ref(source))
        .collect();
    assert_eq!(tags(&completed[..1]), ["AlreadyImported"]);
    let wrong_provider = ImportSource {
        provider: Driver::Claude,
        ..source.clone()
    };
    let wrong: Vec<RecentThread> = scanner
        .recent_threads(&workspace, &[wrong_provider])
        .collect();
    assert_eq!(tags(&wrong[..1]), ["Importable"]);

    // Keep the old inode allocated while replacing the path with an equal-size file.
    let _held = std::fs::File::open(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    write_transcript(
        &path,
        codex_session("replaced-session", &workspace, "Imported prompt"),
        ms(NOW),
    );
    let replaced: Vec<RecentThread> = scanner
        .recent_threads(&workspace, std::slice::from_ref(source))
        .collect();
    let RecentThread::Importable {
        thread,
        source: found,
    } = &replaced[0]
    else {
        panic!("expected an importable thread, got {replaced:?}");
    };
    assert_eq!(thread.session, "replaced-session");
    assert_eq!((found.size, found.mtime_ms), (source.size, source.mtime_ms));
}

#[test]
fn imports_visible_history_from_a_transcript_with_an_oversized_tool_record() {
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    let workspace = temps.dir("workspace-");
    let mut tool = json!({ "type": "tool_result", "data": "" })
        .to_string()
        .into_bytes();
    tool.resize(16 * 1024 * 1024 + 1, b' ');
    let mut contents = format!(
        "{}\n",
        codex_session("large-session", &workspace, "Import this large session")
    )
    .into_bytes();
    contents.extend(tool);
    write_transcript(
        &rollout(&setup.codex, ["2026", "08", "24"], "rollout-large.jsonl"),
        contents,
        ms(NOW),
    );

    let outcomes = setup.recent(&workspace);
    assert_eq!(outcomes.len(), 1);
    let RecentThread::Importable { thread, .. } = &outcomes[0] else {
        panic!("expected an importable thread, got {outcomes:?}");
    };
    assert_eq!(thread.session, "large-session");
    assert_eq!(
        thread
            .messages
            .iter()
            .map(|m| (m.role, m.text.as_str()))
            .collect::<Vec<_>>(),
        [(agent_domain::Role::User, "Import this large session")]
    );
}

#[test]
fn reports_stat_read_and_parse_failures_as_skipped() {
    let mut temps = Temps::default();
    let mut setup = Setup::new(&mut temps);
    let workspace = temps.dir("workspace-");
    let missing = setup.codex.join("missing.jsonl");
    let day = ["2026", "08", "24"];
    let stat_path = rollout(&setup.codex, day, "rollout-stat.jsonl");
    let read_path = rollout(&setup.codex, day, "rollout-read.jsonl");
    let parse_path = rollout(&setup.codex, day, "rollout-parse.jsonl");
    write_transcript(
        &stat_path,
        codex_session("stat-session", &workspace, "Import this session"),
        ms(NOW),
    );
    write_transcript(
        &read_path,
        codex_session("read-session", &workspace, "Import this session"),
        ms(NOW),
    );
    write_transcript(
        &parse_path,
        json!({ "type": "session_meta", "payload": { "id": "parse-session", "cwd": text(&workspace) } }).to_string(),
        ms(NOW),
    );
    let (stats, opens) = (counter(), counter());
    let (stat_missing, open_missing) = (missing.clone(), missing);
    setup.fs = Arc::new(HookFs {
        stat: Some(Box::new(move |path| {
            if path != stat_path {
                return path.to_path_buf();
            }
            if stats.fetch_add(1, Ordering::SeqCst) == 0 {
                path.to_path_buf()
            } else {
                stat_missing.clone()
            }
        })),
        open: Some(Box::new(move |path| {
            if path != read_path {
                return None;
            }
            Some(if opens.fetch_add(1, Ordering::SeqCst) == 0 {
                OsFs.open(path)
            } else {
                OsFs.open(&open_missing)
            })
        })),
        ..HookFs::default()
    });

    assert_eq!(
        setup.recent(&workspace),
        [
            RecentThread::Skipped,
            RecentThread::Skipped,
            RecentThread::Skipped
        ]
    );
}

#[test]
fn does_not_reopen_a_transcript_that_becomes_a_non_file_after_discovery() {
    let mut temps = Temps::default();
    let mut setup = Setup::new(&mut temps);
    let workspace = temps.dir("workspace-");
    let non_file = temps.dir("non-file-");
    let transcript = rollout(&setup.codex, ["2026", "08", "24"], "rollout-changed.jsonl");
    write_transcript(
        &transcript,
        codex_session("changed-session", &workspace, "Do not import this session"),
        ms(NOW),
    );
    let (stats, opens) = (counter(), counter());
    let (stat_target, open_target) = (transcript.clone(), transcript);
    let (stat_count, open_count) = (stats.clone(), opens.clone());
    setup.fs = Arc::new(HookFs {
        stat: Some(Box::new(move |path| {
            if path != stat_target {
                return path.to_path_buf();
            }
            if stat_count.fetch_add(1, Ordering::SeqCst) == 0 {
                path.to_path_buf()
            } else {
                non_file.clone()
            }
        })),
        open: Some(Box::new(move |path| {
            if path == open_target {
                open_count.fetch_add(1, Ordering::SeqCst);
            }
            None
        })),
        ..HookFs::default()
    });

    let outcomes = setup.recent(&workspace);
    assert_eq!(stats.load(Ordering::SeqCst), 2);
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    assert_eq!(outcomes, [RecentThread::Skipped]);
}

#[test]
fn does_not_import_a_transcript_dated_after_the_current_time() {
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    let workspace = temps.dir("workspace-");
    write_transcript(
        &rollout(&setup.codex, ["2026", "08", "24"], "rollout-future.jsonl"),
        codex_session("future-session", &workspace, "Future work"),
        ms(NOW) + 1_000,
    );

    assert_eq!(setup.recent(&workspace), []);
}

#[test]
fn skips_growth_during_reading_without_exceeding_the_reserved_bytes() {
    let mut temps = Temps::default();
    let mut setup = Setup::new(&mut temps);
    let workspace = temps.dir("workspace-");
    let transcript = rollout(&setup.codex, ["2026", "08", "24"], "rollout-growing.jsonl");
    let contents = codex_session(
        "growing-session",
        &workspace,
        "Do not import a changing file",
    );
    write_transcript(&transcript, &contents, ms(NOW));
    let (opens, full_read) = (counter(), counter());
    let grew = Arc::new(AtomicBool::new(false));
    let (open_count, reading, target, original) = (
        opens.clone(),
        full_read.clone(),
        transcript.clone(),
        contents.clone(),
    );
    setup.fs = Arc::new(HookFs {
        open: Some(Box::new(move |path| {
            if path != target {
                return None;
            }
            if open_count.fetch_add(1, Ordering::SeqCst) == 0 {
                return None;
            }
            let (reading, grew, path, original) = (
                reading.clone(),
                grew.clone(),
                path.to_path_buf(),
                original.clone(),
            );
            Some(OsFs.open(&path.clone()).map(|inner| {
                Box::new(Observed {
                    inner,
                    on_read: Box::new(move |_, chunk| {
                        let Some(chunk) = chunk else { return };
                        reading.fetch_add(chunk.len(), Ordering::SeqCst);
                        if !grew.swap(true, Ordering::SeqCst) {
                            std::fs::write(&path, format!("{original}\nchanged")).unwrap();
                        }
                    }),
                }) as Box<dyn TranscriptFile>
            }))
        })),
        ..HookFs::default()
    });

    let outcomes = setup.recent(&workspace);
    assert_eq!(opens.load(Ordering::SeqCst), 2);
    assert_eq!(full_read.load(Ordering::SeqCst), contents.len());
    assert_eq!(outcomes, [RecentThread::Skipped]);
}

#[test]
fn skips_a_transcript_that_shrinks_after_its_size_check() {
    let mut temps = Temps::default();
    let mut setup = Setup::new(&mut temps);
    let workspace = temps.dir("workspace-");
    let transcript = rollout(
        &setup.codex,
        ["2026", "08", "24"],
        "rollout-shrinking.jsonl",
    );
    let shrunk = setup.codex.join("shrunk.jsonl");
    let contents = codex_session(
        "shrinking-session",
        &workspace,
        "Do not import a changing file",
    );
    write_transcript(
        &transcript,
        format!("{contents}\n{}", "padding".repeat(100)),
        ms(NOW),
    );
    write_transcript(&shrunk, &contents, ms(NOW));
    let opens = counter();
    let (open_count, target) = (opens.clone(), transcript);
    setup.fs = Arc::new(HookFs {
        open: Some(Box::new(move |path| {
            if path != target {
                return None;
            }
            Some(if open_count.fetch_add(1, Ordering::SeqCst) == 0 {
                OsFs.open(path)
            } else {
                OsFs.open(&shrunk)
            })
        })),
        ..HookFs::default()
    });

    let outcomes = setup.recent(&workspace);
    assert_eq!(opens.load(Ordering::SeqCst), 2);
    assert_eq!(outcomes, [RecentThread::Skipped]);
}

#[test]
fn does_not_read_the_second_transcript_when_the_consumer_takes_one_thread() {
    let mut temps = Temps::default();
    let mut setup = Setup::new(&mut temps);
    let workspace = temps.dir("workspace-");
    let older = rollout(&setup.codex, ["2026", "08", "23"], "rollout-older.jsonl");
    let newer = rollout(&setup.codex, ["2026", "08", "24"], "rollout-newer.jsonl");
    write_transcript(
        &older,
        codex_session("older-session", &workspace, "Older prompt"),
        ms(NOW) - 1_000,
    );
    write_transcript(
        &newer,
        codex_session("newer-session", &workspace, "Newer prompt"),
        ms(NOW),
    );
    let opens: Arc<Mutex<HashMap<PathBuf, usize>>> = Arc::default();
    let content_reads: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
    let tracked = HashSet::from([older.clone(), newer.clone()]);
    let (opened, reads) = (opens.clone(), content_reads.clone());
    setup.fs = Arc::new(HookFs {
        open: Some(Box::new(move |path| {
            if tracked.contains(path) {
                let mut opens = opened.lock().unwrap();
                let count = opens.entry(path.to_path_buf()).or_default();
                *count += 1;
                if *count == 2 {
                    reads.lock().unwrap().push(path.to_path_buf());
                }
            }
            None
        })),
        ..HookFs::default()
    });

    let threads = importable(
        setup
            .scanner()
            .recent_threads(&workspace, &[])
            .take(1)
            .collect(),
    );
    assert_eq!(
        threads
            .iter()
            .map(|t| t.session.as_str())
            .collect::<Vec<_>>(),
        ["newer-session"]
    );
    assert_eq!(*content_reads.lock().unwrap(), [newer]);
    assert_eq!(opens.lock().unwrap().get(&older), Some(&1));
}

#[test]
fn does_not_import_sessions_from_a_managed_worktree() {
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    let workspace = setup.config_base.join("worktrees/widget/managed-worktree");
    std::fs::create_dir_all(&workspace).unwrap();
    write_transcript(
        &rollout(&setup.codex, ["2026", "08", "24"], "rollout-managed.jsonl"),
        codex_session("managed-session", &workspace, "Do not import this session"),
        ms(NOW),
    );

    assert_eq!(setup.threads(&workspace), []);
}

#[test]
fn uses_one_deterministic_provider_instance_for_a_shared_session_home() {
    let mut temps = Temps::default();
    let mut setup = Setup::new(&mut temps);
    let shared = temps.dir("codex-shared-");
    let workspace = temps.dir("workspace-");
    write_transcript(
        &rollout(&shared, ["2026", "08", "24"], "rollout-shared.jsonl"),
        codex_session("shared-session", &workspace, "Use the shared session"),
        ms(NOW),
    );
    setup.instances = vec![
        (Driver::Codex, "codex", shared.clone()),
        (Driver::Codex, "codex-personal", shared.clone()),
        (Driver::Codex, "codex-work", shared),
    ];

    assert_eq!(
        setup
            .threads(&workspace)
            .iter()
            .map(|t| t.instance.as_str())
            .collect::<Vec<_>>(),
        ["codex"]
    );
}

#[test]
fn uses_configured_order_when_custom_instances_share_a_session_home() {
    let mut temps = Temps::default();
    let mut setup = Setup::new(&mut temps);
    let shared = temps.dir("codex-shared-");
    let workspace = temps.dir("workspace-");
    write_transcript(
        &rollout(&shared, ["2026", "08", "24"], "rollout-shared.jsonl"),
        codex_session("shared-session", &workspace, "Use the first account"),
        ms(NOW),
    );
    setup.instances = vec![
        (Driver::Codex, "codex-work", shared.clone()),
        (Driver::Codex, "codex-personal", shared),
    ];

    assert_eq!(
        setup
            .threads(&workspace)
            .iter()
            .map(|t| t.instance.as_str())
            .collect::<Vec<_>>(),
        ["codex-work"]
    );
}

#[test]
fn keeps_a_second_account_when_the_first_has_5000_newer_files() {
    let mut temps = Temps::default();
    let mut setup = Setup::new(&mut temps);
    let old_workspace = temps.dir("workspace-old-");
    let recent_workspace = temps.dir("workspace-recent-");
    let recent_home = temps.dir("claude-recent-home-");
    let old_directory = setup.claude.join("projects/-aaa-old");
    let old_transcript = old_directory.join("old.jsonl");
    write_transcript(
        &old_transcript,
        json!({ "type": "user", "cwd": text(&old_workspace), "sessionId": "old-session",
                "message": { "role": "user", "content": "Old work" } })
        .to_string(),
        ms(NOW),
    );
    write_transcript(
        &recent_home.join("projects/-zzz-recent/recent.jsonl"),
        json!({ "type": "user", "cwd": text(&recent_workspace), "sessionId": "recent-session",
                "message": { "role": "user", "content": "Recent work" } })
        .to_string(),
        ms(NOW) - 1_000,
    );
    let redirect = {
        let (directory, transcript) = (old_directory.clone(), old_transcript.clone());
        move |path: &Path| {
            let simulated = path.parent() == Some(directory.as_path())
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("old-"));
            if simulated {
                transcript.clone()
            } else {
                path.to_path_buf()
            }
        }
    };
    let (stat_redirect, open_redirect) = (redirect.clone(), redirect);
    setup.fs = Arc::new(HookFs {
        read_dir: Some(Box::new(move |dir| {
            (dir == old_directory).then(|| (0..5_000).map(|i| format!("old-{i}.jsonl")).collect())
        })),
        stat: Some(Box::new(move |path| stat_redirect(path))),
        open: Some(Box::new(move |path| {
            let target = open_redirect(path);
            (target != path).then(|| OsFs.open(&target))
        })),
    });
    setup.instances = vec![(Driver::Claude, "claude-work", recent_home)];

    let scanner = setup.scanner();
    assert!(scanner.scan(&[]).truncated);
    let threads = importable(scanner.recent_threads(&recent_workspace, &[]).collect());
    assert_eq!(
        threads
            .iter()
            .map(|t| t.session.as_str())
            .collect::<Vec<_>>(),
        ["recent-session"]
    );
}

#[test]
fn skips_transcripts_in_excluded_roots_before_any_read() {
    let mut temps = Temps::default();
    let setup = Setup::new(&mut temps);
    write_transcript(
        &rollout(&setup.codex, ["2026", "08", "24"], "rollout-home.jsonl"),
        codex_session("home-session", &setup.home, "Work in home"),
        ms(NOW),
    );

    assert_eq!(setup.recent(&setup.home), []);
    assert_eq!(setup.recent(&std::env::temp_dir()), []);
}
