use super::*;
use std::sync::Mutex;

fn login(name: &str, url: &str, ssh_host: Option<&str>, default: &str) -> Login {
    Login {
        name: name.into(),
        url: url.into(),
        ssh_host: ssh_host.map(str::to_owned),
        valid: None,
        user: "maria".into(),
        default: default.into(),
    }
}

fn identity(remote_url: &str, provider: Option<&str>) -> RepositoryIdentity {
    RepositoryIdentity {
        provider: provider.map(str::to_owned),
        ..super::super::identity("origin", remote_url, "/repo")
    }
}

/// Logins per command-line tool, recording which were listed.
#[derive(Default)]
struct FakeLogins {
    fj: Vec<Login>,
    tea: Vec<Login>,
    listed: Mutex<Vec<(String, Cli, String)>>,
}
impl Logins for FakeLogins {
    fn list<'a>(
        &'a self,
        cwd: &'a str,
        cli: Cli,
        remote_url: &'a str,
    ) -> BoxFuture<'a, Vec<Login>> {
        self.listed
            .lock()
            .unwrap()
            .push((cwd.into(), cli, remote_url.into()));
        Box::pin(async move {
            match cli {
                Cli::Fj => self.fj.clone(),
                Cli::Tea => self.tea.clone(),
            }
        })
    }
}

// SourceControlDiscovery.test.ts "discovers Forgejo accounts and retains the
// server port": the remote refinement.
#[test]
fn refines_an_ssh_remote_by_its_login_ssh_host_and_keeps_the_server_port() {
    let logins = parse_logins(
        r#"[{"name":"work","url":"http://forgejo.local:3000","ssh_host":"git.forgejo.local","user":"maria","default":"true"}]"#,
    );
    let remote = parse_remote("git@git.forgejo.local:maria/project.git").unwrap();
    assert_eq!(
        match_login(&logins, &remote, None, false).map(|login| login.url.as_str()),
        Some("http://forgejo.local:3000")
    );
}

// "does not choose a default Forgejo login across ambiguous SSH server ports"
#[test]
fn does_not_choose_a_default_login_across_ambiguous_ssh_server_ports() {
    let logins = parse_logins(
        &serde_json::to_string(&serde_json::json!([
            {"name": "one", "url": "http://forgejo.local:3000", "ssh_host": "forgejo.local", "user": "maria", "default": "true"},
            {"name": "two", "url": "http://forgejo.local:4000", "ssh_host": "forgejo.local", "user": "maria", "default": "false"},
        ]))
        .unwrap(),
    );
    assert_eq!(logins.len(), 2);
    let remote = parse_remote("git@forgejo.local:maria/project.git").unwrap();
    assert_eq!(
        parse_remote("forgejo.local:maria/project.git"),
        Some(remote.clone())
    );
    assert_eq!(match_login(&logins, &remote, None, false), None);
    assert_eq!(
        match_login(&logins, &remote, Some("forgejo.local:4000"), false).map(|l| l.name.as_str()),
        Some("two")
    );
    assert_eq!(
        match_login(&logins, &remote, Some("other.local:4000"), false),
        None
    );
    let alias = parse_remote("git@ssh.forgejo.local:maria/project.git").unwrap();
    assert_eq!(
        match_login(&logins, &alias, Some("forgejo.local:4000"), false),
        None
    );
    let https = parse_remote("http://forgejo.local:4000/maria/project.git").unwrap();
    assert_eq!(
        match_login(&logins, &https, None, false).map(|l| l.name.as_str()),
        Some("two")
    );
    let host_only = parse_remote("http://forgejo.local:4000").unwrap();
    assert_eq!(
        match_login(&logins, &host_only, None, true).map(|l| l.name.as_str()),
        Some("two")
    );
    let mounted: Vec<Login> = logins
        .iter()
        .map(|login| Login {
            url: format!("http://forgejo.local:4000/{}", login.name),
            ..login.clone()
        })
        .collect();
    assert_eq!(match_login(&mounted, &host_only, None, true), None);
}

#[test]
fn picks_the_default_login_when_every_match_is_the_same_server() {
    let logins = [
        login("a", "https://forge.test", None, "false"),
        login("b", "https://forge.test", None, "true"),
    ];
    let remote = parse_remote("https://forge.test/team/repo.git").unwrap();
    assert_eq!(
        match_login(&logins, &remote, None, false).map(|l| l.name.as_str()),
        Some("b")
    );
    // A later login of the same name replaces the earlier one.
    let renamed = [
        login("a", "https://forge.test", None, "true"),
        login("a", "https://forge.test", None, "false"),
    ];
    assert_eq!(
        match_login(&renamed, &remote, None, false).map(|l| l.default.as_str()),
        Some("false")
    );
}

#[test]
fn parses_remotes_like_the_forgejo_command_line_tools() {
    for (value, expected) in [
        (
            "https://Forge.Test:3000/team/repo.git/",
            Some(("forge.test:3000", "forge.test", false, "team/repo")),
        ),
        (
            "HTTPS://forge.test:443/team/repo",
            Some(("forge.test", "forge.test", false, "team/repo")),
        ),
        (
            "ssh://git@Forge.Test:2222/team/repo.git",
            Some(("forge.test:2222", "forge.test", true, "team/repo")),
        ),
        (
            "git@forge.test:team/repo.git",
            Some(("forge.test", "forge.test", true, "team/repo")),
        ),
        (
            "forge.test:team/repo",
            Some(("forge.test", "forge.test", true, "team/repo")),
        ),
        ("git@forge.test:/team/repo", None),
        ("git://forge.test/team/repo", None),
        ("/home/user/repo", None),
        ("https://", None),
    ] {
        assert_eq!(
            parse_remote(value),
            expected.map(|(host, hostname, ssh, path)| Remote {
                host: host.into(),
                hostname: hostname.into(),
                ssh,
                path: path.into(),
            }),
            "{value}"
        );
    }
}

#[test]
fn lists_nothing_from_malformed_login_output() {
    assert!(parse_logins("not json").is_empty());
    assert!(parse_logins(r#"[{"name":"work"}]"#).is_empty());
    assert!(parse_logins(r#"{"name":"work"}"#).is_empty());
}

#[tokio::test]
async fn gives_a_remote_a_logged_in_server_its_web_address() {
    let logins = FakeLogins {
        tea: vec![login("work", "http://forge.test:3000/git/", None, "true")],
        ..Default::default()
    };
    let refined = refine(
        identity("http://forge.test:3000/git/team/repo.git", Some("unknown")),
        &logins,
    )
    .await;
    assert_eq!(refined.provider.as_deref(), Some("forgejo"));
    assert_eq!(
        refined.web_url.as_deref(),
        Some("http://forge.test:3000/git/team/repo")
    );
    assert_eq!(refined.canonical_key, "forge.test/git/team/repo");
    assert_eq!(
        *logins.listed.lock().unwrap(),
        [
            (
                "/repo".into(),
                Cli::Fj,
                "http://forge.test:3000/git/team/repo.git".into()
            ),
            (
                "/repo".into(),
                Cli::Tea,
                "http://forge.test:3000/git/team/repo.git".into()
            ),
        ]
    );
}

#[tokio::test]
async fn keeps_an_ssh_remote_path_whole_under_a_mounted_server() {
    let logins = FakeLogins {
        fj: vec![login(
            "forge.test",
            "https://forge.test/git",
            Some("ssh.forge.test"),
            "false",
        )],
        ..Default::default()
    };
    let refined = refine(
        identity("git@ssh.forge.test:git/team/repo.git", None),
        &logins,
    )
    .await;
    assert_eq!(
        refined.web_url.as_deref(),
        Some("https://forge.test/git/git/team/repo")
    );
    // fj answered, so tea is not asked.
    assert_eq!(logins.listed.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn confirms_a_forgejo_host_guessed_from_its_name() {
    let logins = FakeLogins {
        fj: vec![login("codeberg.org", "https://codeberg.org", None, "false")],
        ..Default::default()
    };
    let refined = refine(
        identity("https://codeberg.org/team/repo.git", Some("forgejo")),
        &logins,
    )
    .await;
    assert_eq!(
        refined.web_url.as_deref(),
        Some("https://codeberg.org/team/repo")
    );
}

#[tokio::test]
async fn leaves_identities_no_login_serves_or_of_other_providers_as_they_are() {
    let logins = FakeLogins {
        tea: vec![login("work", "https://forge.test", None, "true")],
        ..Default::default()
    };
    for (remote, provider) in [
        ("https://other.test/team/repo.git", Some("unknown")),
        ("https://forge.test/team/repo.git", Some("github")),
        ("/local/mirror", None),
    ] {
        let unrefined = identity(remote, provider);
        assert_eq!(
            refine(unrefined.clone(), &logins).await,
            unrefined,
            "{remote}"
        );
    }
    // Only the unknown remote asked for logins.
    assert_eq!(logins.listed.lock().unwrap().len(), 2);
}

fn keys(json: serde_json::Value) -> Keys {
    serde_json::from_value(json).unwrap()
}

#[test]
fn turns_fj_hosts_and_their_ssh_aliases_into_logins() {
    let stored = keys(serde_json::json!({
        "hosts": {
            "forge.test": {"type": "Application", "token": "secret"},
            "code.test/forgejo": {"type": "OAuth", "token": "secret"},
            "plain.test:3000": {"type": "Application", "token": "secret"},
        },
        "aliases": {"ssh.forge.test": "forge.test", "git.forge.test": "forge.test"},
    }));
    let logins = public_logins(&stored, "git@ssh.forge.test:team/repo.git");
    let summary: Vec<_> = logins
        .iter()
        .map(|login| {
            (
                login.name.as_str(),
                login.url.as_str(),
                login.ssh_host.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("forge.test", "https://forge.test", Some("git.forge.test")),
            ("forge.test", "https://forge.test", Some("ssh.forge.test")),
            ("plain.test:3000", "https://plain.test:3000", None),
        ]
    );
    assert!(
        logins
            .iter()
            .all(|login| login.default == "false" && login.user.is_empty())
    );
    // Only an explicit matching HTTP remote selects HTTP.
    let http = public_logins(&stored, "HTTP://plain.test:3000/team/repo.git");
    assert_eq!(http[2].url, "http://plain.test:3000");
    let other = public_logins(&stored, "http://elsewhere.test/team/repo.git");
    assert_eq!(other[2].url, "https://plain.test:3000");
}

#[test]
fn rejects_fj_keys_of_another_shape() {
    for invalid in [
        serde_json::json!({"hosts": {"forge.test": {"type": "Password", "token": "x"}}}),
        serde_json::json!({"hosts": {"forge.test": {"type": "Application"}}}),
        serde_json::json!({"aliases": {}}),
    ] {
        assert!(
            serde_json::from_value::<Keys>(invalid.clone()).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn finds_fj_keys_where_fj_keeps_them() {
    let home = Path::new("/home/maria");
    assert_eq!(
        keys_paths("macos", home, None, None),
        [
            home.join("Library/Application Support/forgejo-cli.forgejo-cli/keys.json"),
            home.join("Library/Application Support/Cyborus.forgejo-cli/keys.json"),
        ]
    );
    assert_eq!(
        keys_paths("linux", home, Some("/data"), None),
        [Path::new("/data/forgejo-cli/keys.json")]
    );
    assert_eq!(
        keys_paths("linux", home, Some("relative"), None),
        [home.join(".local/share/forgejo-cli/keys.json")]
    );
    assert_eq!(
        keys_paths("windows", home, None, Some("/roaming")),
        [
            Path::new("/roaming/forgejo-cli/forgejo-cli/data/keys.json"),
            Path::new("/roaming/Cyborus/forgejo-cli/data/keys.json"),
        ]
    );
    assert_eq!(
        keys_paths("windows", home, None, None)[0],
        home.join("AppData/Roaming/forgejo-cli/forgejo-cli/data/keys.json")
    );
}

#[tokio::test]
async fn reads_the_first_fj_keys_file_there_is() {
    let directory = tempfile::tempdir().unwrap();
    let (missing, present) = (
        directory.path().join("missing.json"),
        directory.path().join("keys.json"),
    );
    std::fs::write(
        &present,
        r#"{"hosts":{"forge.test":{"type":"Application","token":"secret"}}}"#,
    )
    .unwrap();
    let logins = CliLogins {
        keys: vec![missing.clone(), present.clone()],
        tea: directory.path().join("no-tea"),
    };
    let listed = logins
        .list("/", Cli::Fj, "https://forge.test/team/repo")
        .await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].url, "https://forge.test");
    // No keys file lists nothing; an unreadable one too.
    let none = CliLogins {
        keys: vec![missing],
        tea: directory.path().join("no-tea"),
    };
    assert!(none.list("/", Cli::Fj, "").await.is_empty());
    std::fs::write(&present, "not json").unwrap();
    assert!(logins.list("/", Cli::Fj, "").await.is_empty());
    // A missing tea lists nothing.
    assert!(logins.list("/", Cli::Tea, "").await.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn lists_tea_logins_from_its_json_output() {
    use std::os::unix::fs::PermissionsExt as _;
    let directory = tempfile::tempdir().unwrap();
    let tea = directory.path().join("tea");
    std::fs::write(
        &tea,
        "#!/bin/sh\n[ \"$*\" = \"login list --output json\" ] || exit 1\n\
         echo '[{\"name\":\"work\",\"url\":\"http://forge.test:3000\",\"user\":\"maria\",\"default\":\"true\"}]'\n",
    )
    .unwrap();
    std::fs::set_permissions(&tea, std::fs::Permissions::from_mode(0o755)).unwrap();
    let logins = CliLogins { keys: vec![], tea };
    let listed = logins
        .list(directory.path().to_str().unwrap(), Cli::Tea, "")
        .await;
    assert_eq!(
        listed,
        [login("work", "http://forge.test:3000", None, "true")]
    );
}

proptest::proptest! {
    // Every remote form names the same server and repository path.
    #[test]
    fn parses_each_remote_form_to_its_server_and_path(
        host in "[a-z][a-z0-9-]{0,8}(\\.[a-z][a-z0-9]{0,5}){0,2}",
        port in proptest::option::of(1024u16..65535),
        segments in proptest::collection::vec("[A-Za-z0-9_][A-Za-z0-9_.-]{0,8}", 1..4),
        suffix in proptest::bool::ANY,
    ) {
        let path = segments.join("/");
        proptest::prop_assume!(!path.ends_with(".git"));
        let written = if suffix { format!("{path}.git") } else { path.clone() };
        let with_port = port.map_or_else(|| host.clone(), |port| format!("{host}:{port}"));
        for (value, expected_host, ssh) in [
            (format!("https://{with_port}/{written}"), with_port.clone(), false),
            (format!("ssh://git@{with_port}/{written}"), with_port.clone(), true),
            (format!("git@{host}:{written}"), host.clone(), true),
        ] {
            let parsed = parse_remote(&value);
            proptest::prop_assert_eq!(
                parsed,
                Some(Remote {
                    host: expected_host,
                    hostname: host.clone(),
                    ssh,
                    path: path.clone(),
                }),
                "{}",
                value
            );
        }
    }

    // A login serves a remote of its own server under its mount, and no other.
    #[test]
    fn matches_only_logins_of_the_remote_server(
        mount in proptest::option::of("[a-z]{1,6}"),
        repository in "[a-z]{1,6}/[a-z]{1,6}",
        other_port in 1024u16..65535,
    ) {
        let base = mount.as_ref().map_or_else(
            || "https://forge.test".to_owned(),
            |mount| format!("https://forge.test/{mount}"),
        );
        let logins = [login("work", &base, None, "false")];
        let served = parse_remote(&format!("{base}/{repository}.git")).unwrap();
        proptest::prop_assert_eq!(
            match_login(&logins, &served, None, false).map(|l| l.name.as_str()),
            Some("work")
        );
        let elsewhere =
            parse_remote(&format!("https://forge.test:{other_port}/{repository}.git")).unwrap();
        proptest::prop_assert_eq!(match_login(&logins, &elsewhere, None, false), None);
    }
}
