//! Projects started from just a name: each is a new Git repository the Host makes
//! in its own `projects` folder, with a README, an icon and a first commit.
use super::{ProjectStore, Registration};
use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    process::{Output, Stdio},
    time::Duration,
};

const MAX_FOLDER_ATTEMPTS: usize = 100;

/// Tailwind 600 shades: dark enough for white initials on every hue.
const ICON_BACKGROUNDS: [&str; 15] = [
    "#dc2626", "#ea580c", "#d97706", "#16a34a", "#059669", "#0d9488", "#0891b2", "#0284c7",
    "#2563eb", "#4f46e5", "#7c3aed", "#9333ea", "#c026d3", "#db2777", "#e11d48",
];

/// A project started from a name: its id, folder, and why its first commit
/// failed, if it did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NamedProject {
    pub(crate) id: String,
    pub(crate) root: PathBuf,
    pub(crate) commit_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NamedProjectError {
    /// The folder or its repository could not be made.
    Folder,
    /// The project could not be registered.
    Create,
}
impl NamedProjectError {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Folder => "Failed to create the project folder.",
            Self::Create => "Failed to create the project.",
        }
    }
}

/// How Git runs. Tests replace the program or isolate its configuration; an
/// environment value of `None` removes the variable.
#[derive(Debug, Clone)]
pub(crate) struct Git {
    program: PathBuf,
    env: Vec<(OsString, Option<OsString>)>,
}
impl Default for Git {
    fn default() -> Self {
        Self {
            program: "git".into(),
            env: vec![],
        }
    }
}
impl Git {
    async fn run(&self, cwd: &Path, args: &[&str], timeout: Duration) -> io::Result<Output> {
        let mut command = tokio::process::Command::new(&self.program);
        command
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        for (key, value) in &self.env {
            match value {
                Some(value) => command.env(key, value),
                None => command.env_remove(key),
            };
        }
        tokio::time::timeout(timeout, command.output())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Git timed out."))?
    }
    async fn execute(&self, cwd: &Path, args: &[&str], timeout: Duration) -> io::Result<()> {
        let output = self.run(cwd, args, timeout).await?;
        if output.status.success() {
            Ok(())
        } else {
            Err(io::Error::other(crate::git::failure(&output)))
        }
    }
}

/// The folder name for a project started from a name: "Pinball Stats" becomes
/// "pinball-stats". Only `[a-z0-9-]` remains, so it stays one path segment.
pub(crate) fn new_project_folder_name(name: &str) -> String {
    let decomposed = icu_normalizer::DecomposingNormalizer::new_nfkd().normalize(name);
    let lowered = decomposed
        .chars()
        .filter(|c| !('\u{0300}'..='\u{036f}').contains(c))
        .collect::<String>()
        .to_lowercase();
    let mut slug = String::new();
    for c in lowered.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_start_matches('-').chars().take(64).collect();
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        return "project".into();
    }
    // Windows refuses these as file names, with or without an extension.
    let reserved = matches!(slug, "con" | "prn" | "aux" | "nul")
        || (slug.len() == 4
            && (slug.starts_with("com") || slug.starts_with("lpt"))
            && matches!(slug.as_bytes()[3], b'1'..=b'9'));
    if reserved {
        format!("{slug}-project")
    } else {
        slug.to_owned()
    }
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A rounded square with the name's initials, colored by a hash of the name. It
/// lives at `assets/icon.svg`, so every clone shows the same icon.
fn icon_svg(name: &str) -> String {
    static SEPARATORS: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"[^\p{L}\p{N}]+").expect("valid pattern"));
    let initials: String = SEPARATORS
        .split(name)
        .filter(|word| !word.is_empty())
        .take(2)
        .filter_map(|word| word.chars().next())
        .collect::<String>()
        .to_uppercase();
    let initials = if initials.is_empty() {
        name.trim()
            .chars()
            .next()
            .map_or_else(|| "?".to_owned(), String::from)
    } else {
        initials
    };
    let hash = name
        .chars()
        .fold(0u32, |hash, c| hash.wrapping_mul(31).wrapping_add(c as u32));
    let background = ICON_BACKGROUNDS[hash as usize % ICON_BACKGROUNDS.len()];
    let size = if initials.chars().count() > 1 { 26 } else { 32 };
    [
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64">"#.to_owned(),
        format!(r#"  <rect width="64" height="64" rx="14" fill="{background}"/>"#),
        format!(
            r##"  <text x="32" y="32" dy="0.35em" text-anchor="middle" font-family="ui-sans-serif, system-ui, -apple-system, sans-serif" font-size="{size}" font-weight="600" fill="#ffffff">{}</text>"##,
            escape_xml(&initials)
        ),
        "</svg>".to_owned(),
        String::new(),
    ]
    .join("\n")
}

fn readme(name: &str) -> String {
    [
        r#"<img src="assets/icon.svg" width="64" height="64" alt="">"#.to_owned(),
        String::new(),
        format!("# {name}"),
        String::new(),
        "Created in Bex.".to_owned(),
        String::new(),
    ]
    .join("\n")
}

/// Git's own identity message runs several lines, so say what to do instead.
/// Otherwise its last line ("error: gpg failed to sign the data") says enough.
fn describe_commit_failure(stderr: &str) -> String {
    let lower = stderr.to_lowercase();
    if lower.contains("identity unknown")
        || lower.contains("tell me who you are")
        || lower.contains("no name was given")
        || lower.contains("no email was given")
    {
        return "Git has no name or email on this machine. Set user.name and user.email, then commit.".into();
    }
    stderr
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .map_or_else(
            || "Git could not make the first commit.".to_owned(),
            str::to_owned,
        )
}

/// Claims the first free folder among `<root>/<name>`, `<root>/<name>-2`, ...
/// Each is created without its parents, so creating it is the claim.
fn claim_folder(root: &Path, name: &str) -> io::Result<Option<PathBuf>> {
    crate::platform::create_state_directory(root)?;
    for attempt in 1..=MAX_FOLDER_ATTEMPTS {
        let folder = match attempt {
            1 => root.join(name),
            _ => root.join(format!("{name}-{attempt}")),
        };
        match std::fs::create_dir(&folder) {
            Ok(()) => return Ok(Some(folder)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

/// `git init`, the starter files and the first commit. Only the commit may fail
/// softly, returning why.
async fn scaffold(cwd: &Path, name: &str, git: &Git) -> io::Result<Option<String>> {
    let branch = git
        .run(
            cwd,
            &["config", "--get", "init.defaultBranch"],
            Duration::from_secs(10),
        )
        .await
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|branch| !branch.is_empty())
        .unwrap_or_else(|| "main".into());
    git.execute(
        cwd,
        &["init", &format!("--initial-branch={branch}")],
        Duration::from_secs(10),
    )
    .await?;
    tokio::fs::write(cwd.join("README.md"), readme(name)).await?;
    tokio::fs::create_dir(cwd.join("assets")).await?;
    tokio::fs::write(cwd.join("assets").join("icon.svg"), icon_svg(name)).await?;
    // Named and forced so a global ignore rule (say `*.svg`) cannot drop one.
    git.execute(
        cwd,
        &["add", "--force", "--", "README.md", "assets/icon.svg"],
        Duration::from_secs(10),
    )
    .await?;
    Ok(
        match git
            .run(
                cwd,
                &["commit", "--message", "Initial commit"],
                Duration::from_secs(30),
            )
            .await
        {
            Ok(output) if output.status.success() => None,
            Ok(output) => Some(describe_commit_failure(&String::from_utf8_lossy(
                &output.stderr,
            ))),
            Err(error) => Some(error.to_string()),
        },
    )
}

/// Removes a claimed folder unless kept; a dropped create removes it too.
struct Claimed(Option<PathBuf>);
impl Claimed {
    fn keep(&mut self) -> PathBuf {
        self.0.take().expect("kept once")
    }
}
impl Drop for Claimed {
    fn drop(&mut self) {
        if let Some(folder) = self.0.take() {
            let _ = std::fs::remove_dir_all(folder);
        }
    }
}

/// Whether a registered project has `folder` as its root.
async fn owned(store: &ProjectStore, folder: &Path) -> bool {
    let Ok(folder) = tokio::fs::canonicalize(folder).await else {
        return false;
    };
    let folder = dunce::simplified(&folder).to_path_buf();
    let Ok(projects) = store.load().await else {
        // An unreadable store may still name the folder; never delete it then.
        return true;
    };
    for root in projects.iter().flat_map(|project| &project.roots) {
        if let Ok(root) = tokio::fs::canonicalize(&root.path).await
            && dunce::simplified(&root) == folder
        {
            return true;
        }
    }
    false
}

/// Starts a project from just a name: claims `<projects>/<slug>` (adding `-2`,
/// `-3`, ... when taken), makes it a Git repository with a README, an icon and a
/// first commit, then registers it under the name. A failed commit keeps the
/// project and says why. The folder is removed when the create fails or is
/// dropped before the project exists, never once another project owns it.
pub(crate) async fn create_named_project(
    store: &ProjectStore,
    name: &str,
    git: &Git,
) -> Result<NamedProject, NamedProjectError> {
    let root = store.named_project_directory();
    let folder_name = new_project_folder_name(name);
    let folder = tokio::task::spawn_blocking(move || claim_folder(&root, &folder_name))
        .await
        .map_err(|_| NamedProjectError::Folder)?
        .map_err(|_| NamedProjectError::Folder)?
        .ok_or(NamedProjectError::Folder)?;
    let mut claimed = Claimed(Some(folder.clone()));
    let commit_error = scaffold(&folder, name, git)
        .await
        .map_err(|_| NamedProjectError::Folder)?;
    // From here the create may commit even if this future is dropped, so the
    // folder is removed only after a failure that left it unowned.
    let folder = claimed.keep();
    match store.add(&folder, Some(name), vec![]).await {
        Ok(Registration::Created(id)) => Ok(NamedProject {
            id,
            root: folder,
            commit_error,
        }),
        // Another project owns the folder.
        Ok(Registration::Existing(_)) => Err(NamedProjectError::Create),
        Err(_) => {
            if !owned(store, &folder).await {
                let _ = tokio::fs::remove_dir_all(&folder).await;
            }
            Err(NamedProjectError::Create)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // path.test.ts "derives the folder name of a project started from a name".
    #[test]
    fn derives_the_folder_name_of_a_project_started_from_a_name() {
        assert_eq!(new_project_folder_name("Pinball Stats"), "pinball-stats");
        assert_eq!(new_project_folder_name("  Café & Crème!  "), "cafe-creme");
        assert_eq!(new_project_folder_name("../../etc"), "etc");
        // Nothing usable left, so the folder falls back to a fixed name.
        assert_eq!(new_project_folder_name("🎱🎱"), "project");
        assert_eq!(
            new_project_folder_name(&format!("{} b", "a".repeat(63))),
            "a".repeat(63)
        );
        // Windows cannot make folders with device names.
        assert_eq!(new_project_folder_name("Con"), "con-project");
        assert_eq!(new_project_folder_name("LPT1"), "lpt1-project");
        assert_eq!(new_project_folder_name("console"), "console");
    }

    proptest! {
        #[test]
        fn folder_names_are_one_short_portable_segment(name in "\\PC{0,80}") {
            let folder = new_project_folder_name(&name);
            prop_assert!(!folder.is_empty());
            prop_assert!(folder.len() <= 64 + "-project".len());
            prop_assert!(folder.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'));
            prop_assert!(!folder.starts_with('-') && !folder.ends_with('-'));
            prop_assert!(!folder.contains("--"));
            prop_assert!(!["con", "prn", "aux", "nul", "com1", "lpt9"].contains(&folder.as_str()));
        }

        #[test]
        fn lowercase_words_keep_their_text(words in proptest::collection::vec("[a-z0-9]{1,8}", 1..6)) {
            let name = words.join(" ");
            let expected = words.join("-");
            prop_assume!(expected.len() <= 64);
            let folder = new_project_folder_name(&name.to_uppercase());
            let reserved = format!("{}-project", expected);
            prop_assert!(folder == expected || folder == reserved);
        }
    }

    #[test]
    fn icons_show_up_to_two_initials_and_readmes_link_the_icon() {
        assert!(
            icon_svg("Pinball Stats")
                .contains(r##"font-size="26" font-weight="600" fill="#ffffff">PS</text>"##)
        );
        assert!(icon_svg("notes").contains(">N</text>"));
        assert!(icon_svg("a & b <c>").contains(">AB</text>"));
        assert!(
            icon_svg("🎱")
                .contains(r##"font-size="32" font-weight="600" fill="#ffffff">🎱</text>"##)
        );
        // The name's hash picks the color: "ab" hashes to 97 * 31 + 98.
        assert!(icon_svg("ab").contains(&format!(
            r#"fill="{}""#,
            ICON_BACKGROUNDS[(97 * 31 + 98) % ICON_BACKGROUNDS.len()]
        )));
        let readme = readme("Pinball Stats");
        assert!(readme.contains("# Pinball Stats"));
        assert!(readme.contains(r#"src="assets/icon.svg""#));
    }

    #[test]
    fn describes_why_the_first_commit_failed() {
        assert_eq!(
            describe_commit_failure("Author identity unknown\n\n*** Please tell me who you are.\n"),
            "Git has no name or email on this machine. Set user.name and user.email, then commit."
        );
        assert_eq!(
            describe_commit_failure("fatal: no email was given and auto-detection is disabled\n"),
            "Git has no name or email on this machine. Set user.name and user.email, then commit."
        );
        assert_eq!(
            describe_commit_failure(
                "error: gpg failed to sign the data\nfatal: failed to write commit object\n\n"
            ),
            "fatal: failed to write commit object"
        );
        assert_eq!(
            describe_commit_failure(" \n"),
            "Git could not make the first commit."
        );
    }

    /// Git reading only an empty global configuration plus `env`, so the
    /// developer's own identity and signing settings stay out of the test.
    fn isolated_git(home: &Path, env: &[(&str, &str)]) -> Git {
        let config = home.join("empty.gitconfig");
        std::fs::write(&config, "").unwrap();
        let mut git = Git::default();
        for key in [
            "GIT_AUTHOR_NAME",
            "GIT_AUTHOR_EMAIL",
            "GIT_COMMITTER_NAME",
            "GIT_COMMITTER_EMAIL",
            "EMAIL",
        ] {
            git.env.push((key.into(), None));
        }
        git.env
            .push(("GIT_CONFIG_GLOBAL".into(), Some(config.into_os_string())));
        git.env
            .push(("GIT_CONFIG_NOSYSTEM".into(), Some("1".into())));
        for (key, value) in env {
            git.env.push(((*key).into(), Some((*value).into())));
        }
        git
    }
    const IDENTITY: [(&str, &str); 4] = [
        ("GIT_AUTHOR_NAME", "Test"),
        ("GIT_AUTHOR_EMAIL", "test@test.com"),
        ("GIT_COMMITTER_NAME", "Test"),
        ("GIT_COMMITTER_EMAIL", "test@test.com"),
    ];
    fn git_output(git: &Git, cwd: &Path, args: &[&str]) -> String {
        let mut command = std::process::Command::new(&git.program);
        command.args(args).current_dir(cwd);
        for (key, value) in &git.env {
            match value {
                Some(value) => command.env(key, value),
                None => command.env_remove(key),
            };
        }
        let output = command.output().unwrap();
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }
    fn data_directory() -> (tempfile::TempDir, ProjectStore) {
        let directory = tempfile::tempdir().unwrap();
        let data = dunce::canonicalize(directory.path()).unwrap();
        let store = ProjectStore::new(data.join("worktrees.json"));
        (directory, store)
    }
    fn folders(store: &ProjectStore) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(store.named_project_directory())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    // ManagedProjectFolders.test.ts "starts a named project as a committed
    // repository, and suffixes a taken name".
    #[tokio::test]
    async fn starts_a_named_project_as_a_committed_repository_and_suffixes_a_taken_name() {
        let (directory, store) = data_directory();
        let git = isolated_git(directory.path(), &IDENTITY);
        let root = store.named_project_directory();
        assert_eq!(root, store.path().with_file_name("projects"));

        let first = create_named_project(&store, "Pinball Stats", &git)
            .await
            .unwrap();
        let second = create_named_project(&store, "pinball stats", &git)
            .await
            .unwrap();

        assert_eq!(first.root, root.join("pinball-stats"));
        assert_eq!(second.root, root.join("pinball-stats-2"));
        assert_eq!(first.commit_error, None);
        let projects = store.load().await.unwrap();
        let project = projects.iter().find(|p| p.id == first.id).unwrap();
        assert_eq!(project.name, "Pinball Stats");
        assert_eq!(project.roots[0].path, first.root.to_str().unwrap());

        let readme = std::fs::read_to_string(first.root.join("README.md")).unwrap();
        assert!(readme.contains("# Pinball Stats"));
        assert!(readme.contains(r#"src="assets/icon.svg""#));
        let icon = std::fs::read_to_string(first.root.join("assets").join("icon.svg")).unwrap();
        assert!(icon.contains(">PS</text>"));
        assert_eq!(
            git_output(&git, &first.root, &["log", "--format=%s"]),
            "Initial commit"
        );
        assert_eq!(
            git_output(&git, &first.root, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "main"
        );
        assert_eq!(
            git_output(&git, &first.root, &["status", "--porcelain"]),
            ""
        );
    }

    // "gives concurrent named projects with the same name distinct folders".
    #[tokio::test]
    async fn gives_concurrent_named_projects_with_the_same_name_distinct_folders() {
        let (directory, store) = data_directory();
        let git = isolated_git(directory.path(), &IDENTITY);
        let created = futures_util::future::join_all(
            (0..4).map(|_| create_named_project(&store, "Race", &git)),
        )
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
        let roots: std::collections::HashSet<_> = created.iter().map(|p| &p.root).collect();
        let ids: std::collections::HashSet<_> = created.iter().map(|p| &p.id).collect();
        assert_eq!((roots.len(), ids.len()), (4, 4));
        let projects = store.load().await.unwrap();
        for result in &created {
            assert!(
                projects
                    .iter()
                    .any(|p| p.roots[0].path == result.root.to_str().unwrap())
            );
        }
    }

    // "keeps a named project and reports why when Git cannot commit".
    #[tokio::test]
    async fn keeps_a_named_project_and_reports_why_when_git_cannot_commit() {
        let (directory, store) = data_directory();
        // No identity anywhere, and Git may not guess one from the host name.
        let no_identity = directory.path().join("no-identity.gitconfig");
        std::fs::write(&no_identity, "[user]\n\tuseConfigOnly = true\n").unwrap();
        let mut git = isolated_git(directory.path(), &[]);
        git.env.push((
            "GIT_CONFIG_GLOBAL".into(),
            Some(no_identity.into_os_string()),
        ));

        let result = create_named_project(&store, "No Identity", &git)
            .await
            .unwrap();

        assert!(
            result
                .commit_error
                .as_deref()
                .unwrap_or_default()
                .contains("no name or email")
        );
        assert!(
            store
                .load()
                .await
                .unwrap()
                .iter()
                .any(|p| p.id == result.id)
        );
        assert!(result.root.join("README.md").exists());
        assert!(result.root.join(".git").exists());
    }

    // "keeps a folder that another project owns when the create conflicts".
    #[tokio::test]
    async fn keeps_a_folder_that_another_project_owns_when_the_create_conflicts() {
        let (directory, store) = data_directory();
        let git = isolated_git(directory.path(), &IDENTITY);
        let taken = store.named_project_directory().join("taken");
        // A project registered at this path some other way owns the folder the
        // create claims.
        std::fs::create_dir_all(&taken).unwrap();
        let Registration::Created(owner) = store.add(&taken, Some("Owner"), vec![]).await.unwrap()
        else {
            panic!("the owner is created");
        };
        std::fs::remove_dir_all(&taken).unwrap();

        let failure = create_named_project(&store, "Taken", &git)
            .await
            .unwrap_err();

        assert_eq!(failure, NamedProjectError::Create);
        assert_eq!(failure.message(), "Failed to create the project.");
        assert!(taken.join("README.md").exists());
        let projects = store.load().await.unwrap();
        assert_eq!(
            projects
                .iter()
                .map(|p| (p.id.as_str(), p.roots[0].path.as_str()))
                .collect::<Vec<_>>(),
            [(owner.as_str(), taken.to_str().unwrap())]
        );
        assert_eq!(folders(&store), ["taken"]);
    }

    // "removes the folder when the project create is rejected for another reason".
    #[cfg(unix)]
    #[tokio::test]
    async fn removes_the_folder_when_the_project_create_is_rejected_for_another_reason() {
        use std::os::unix::fs::PermissionsExt;
        let (directory, store) = data_directory();
        let git = isolated_git(directory.path(), &IDENTITY);
        // The project list cannot be saved beside a read-only data directory.
        std::fs::create_dir(store.named_project_directory()).unwrap();
        let data = store.path().parent().unwrap().to_path_buf();
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o555)).unwrap();

        let failure = create_named_project(&store, "Rejected", &git).await;

        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(failure.unwrap_err(), NamedProjectError::Create);
        assert!(folders(&store).is_empty());
        assert!(store.load().await.unwrap().is_empty());
    }

    // "removes the folder when the create is cancelled before the project exists".
    #[cfg(unix)]
    #[tokio::test]
    async fn removes_the_folder_when_the_create_is_cancelled_before_the_project_exists() {
        use std::os::unix::fs::PermissionsExt;
        let (directory, store) = data_directory();
        // A Git that never finishes holds the create mid-scaffold.
        let program = directory.path().join("hanging-git");
        std::fs::write(&program, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let git = Git {
            program,
            env: vec![],
        };
        let creating = {
            let store = store.clone();
            tokio::spawn(async move { create_named_project(&store, "Cancelled", &git).await })
        };
        let folder = store.named_project_directory().join("cancelled");
        while !folder.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        creating.abort();
        assert!(creating.await.unwrap_err().is_cancelled());

        assert!(folders(&store).is_empty());
        assert!(store.load().await.unwrap().is_empty());
    }

    // "removes the folder when the repository cannot be made".
    #[tokio::test]
    async fn removes_the_folder_when_the_repository_cannot_be_made() {
        let (directory, store) = data_directory();
        // A missing Git cannot make the repository.
        let git = Git {
            program: directory.path().join("missing-git"),
            env: vec![],
        };

        let failure = create_named_project(&store, "Broken", &git)
            .await
            .unwrap_err();

        assert_eq!(failure, NamedProjectError::Folder);
        assert_eq!(failure.message(), "Failed to create the project folder.");
        assert!(folders(&store).is_empty());
        assert!(store.load().await.unwrap().is_empty());
    }
}
