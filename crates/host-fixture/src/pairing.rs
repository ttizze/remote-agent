//! Loopback-only UI test controls. No saved application profiles are used.

use crate::{Result, test_support::Connection};
use agent_protocol::operations::{CreateInvitation, IdentifyDevice, Pair};
use agent_transport::transport::{Identity, Ticket};
use serde_json::json;
use std::fs;
use std::{
    fs::OpenOptions,
    io::Write,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};
use tiny_http::{Header, Method, Response, Server};

/// A page the clients' browsers load without the network. Its title, which a
/// browser's tab list shows, repeats what is typed into its focused field.
const BROWSER_PAGE: &str = "/browser-page";
const BROWSER_PAGE_HTML: &str = r#"<!doctype html><html lang="ja"><head><meta charset="utf-8">
<title>検証ページ</title></head><body><h1>検証ページ</h1>
<label>入力欄 <input id="field" autofocus></label>
<script>field.oninput = () => document.title = "入力 " + field.value;</script>
</body></html>"#;

pub struct PairingServer {
    pub port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<()>>>,
}

impl PairingServer {
    pub fn start(
        root: &Path,
        projects: host_daemon::ProjectStore,
        ticket: Ticket,
        identity: Identity,
    ) -> Result<Self> {
        let root = dunce::canonicalize(root)?;
        let server = Server::http((Ipv4Addr::LOCALHOST, 0))?;
        let port = server
            .server_addr()
            .to_ip()
            .ok_or("pairing server did not bind an IP socket")?
            .port();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            while !stopped.load(Ordering::Acquire) {
                let Some(request) = server.recv_timeout(Duration::from_millis(100))? else {
                    continue;
                };
                let page = request.method() == &Method::Get && request.url() == BROWSER_PAGE;
                let (status, body) = if page {
                    (200, BROWSER_PAGE_HTML.as_bytes().to_vec())
                } else {
                    route(
                        &root,
                        &projects,
                        &runtime,
                        &ticket,
                        &identity,
                        request.method(),
                        request.url(),
                    )
                    .unwrap_or_else(|_| (503, b"Isolated Host is unavailable".to_vec()))
                };
                let kind = if page {
                    "text/html; charset=utf-8"
                } else {
                    "application/json; charset=utf-8"
                };
                let response = Response::from_data(body)
                    .with_status_code(status)
                    .with_header(Header::from_bytes("Content-Type", kind).unwrap())
                    .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
                // A disconnected Simulator request must not stop the fixture.
                let _ = request.respond(response);
            }
            Ok(())
        });
        Ok(Self {
            port,
            stop,
            thread: Some(thread),
        })
    }

    pub fn shutdown(mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        self.thread
            .take()
            .unwrap()
            .join()
            .map_err(|_| "pairing server thread panicked")?
    }
}

impl Drop for PairingServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn rpc<O: agent_protocol::operations::RpcMethod>(
    runtime: &tokio::runtime::Runtime,
    ticket: &Ticket,
    identity: &Identity,
    operation: &O,
) -> Result<O::Output> {
    runtime.block_on(async {
        let connection =
            Connection::open(ticket, Identity::from_bytes(identity.to_bytes())).await?;
        let response = connection.peer.call(operation).await;
        connection.close().await;
        response.map_err(Into::into)
    })
}

/// `/<kind>-conversation/<id>`: lists one conversation whose history is the
/// fixture's long history of that kind. False for other paths.
fn history_conversation(root: &Path, path: &str) -> Result<bool> {
    let Some((kind, id)) = path.rsplit_once('/') else {
        return Ok(false);
    };
    let (history, name) = match kind {
        "/long-conversation" => ("long", "Long conversation"),
        "/viewport-conversation" => ("viewport", "Long conversation"),
        "/reading-conversation" => ("reading", "Reading conversation"),
        "/render-conversation" => ("render", "Long conversation"),
        // The long history with its latest turn still working.
        "/running-conversation" => ("running", "Long conversation"),
        _ => return Ok(false),
    };
    write_json(
        root.join("list-fixture.json"),
        &json!([{"id":id,"fixtureHistory":history,
            "cwd":root.join("project"),"name":name,
            "createdAt":10000,"updatedAt":10000,"status":{"type":"notLoaded"},"historyMode":"paginated"}]),
    )?;
    Ok(true)
}

fn route(
    root: &Path,
    projects: &host_daemon::ProjectStore,
    runtime: &tokio::runtime::Runtime,
    ticket: &Ticket,
    identity: &Identity,
    method: &Method,
    path: &str,
) -> Result<(u16, Vec<u8>)> {
    if method == &Method::Get && path == "/pairing" {
        return Ok((
            200,
            serde_json::to_vec(&rpc(runtime, ticket, identity, &CreateInvitation {})?)?,
        ));
    }
    if method != &Method::Post {
        return Ok((404, Vec::new()));
    }
    if let Some(native) = path.strip_prefix("/session-identity/") {
        return Ok((
            200,
            serde_json::to_vec(&json!({"threadId":session_identity(root, native)?}))?,
        ));
    }
    // Each test names its own worktree conversation so attachments and
    // checkout state from one test never reach another on a shared Host.
    if let Some(name) = path.strip_prefix("/worktree-conversation/") {
        worktree_conversation(root, name)?;
        return Ok((204, Vec::new()));
    }
    if let Some((name, state)) = path
        .strip_prefix("/worktree/")
        .and_then(|rest| rest.split_once('/'))
    {
        let readable = match state {
            "unavailable" => false,
            "restore" => true,
            _ => return Err("unknown worktree fixture state".into()),
        };
        // A deleted worktree is recreated for input; unreadable directories make review fail.
        set_worktree_access(&root.join(format!("{name}-worktree")), readable)?;
        return Ok((204, Vec::new()));
    }
    if history_conversation(root, path)? {
        return Ok((204, Vec::new()));
    }
    match path {
        "/uploads/unavailable" | "/uploads/restore" => {
            set_upload_storage_available(root, path == "/uploads/restore")?;
        }
        "/auth-token/unavailable" => fs::write(root.join("auth-token-unavailable"), [])?,
        "/auth-token/reset" => {
            if root.join("auth-token-unavailable").exists() {
                fs::remove_file(root.join("auth-token-unavailable"))?;
            }
        }
        "/merge-worktree/fresh"
        | "/merge-worktree/merged"
        | "/merge-worktree/new-work"
        | "/merge-worktree/dirty"
        | "/merge-worktree/clean" => merge_worktree(root, path)?,
        // A changed file Git has no text diff for, which the diff names binary.
        "/binary-file" => fs::write(root.join("project").join("pixel.bin"), [0, 1, 2, 255, 0])?,
        // The Codex fixture's background command and subagent finish.
        "/codex-background/release" => fs::write(root.join("codex-background-release"), [])?,
        // The Claude fixture's background work finishes, and Claude reports on it.
        "/claude-background/release" => {
            fs::write(root.join("project/claude-background-release"), [])?;
        }
        "/release-inputs" => {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(root.join("release-inputs"))?;
        }
        "/external-conversation" => {
            let rollout = root.join("external-rollout.jsonl");
            fs::write(&rollout, "initial persisted history\n")?;
            write_json(
                root.join("list-fixture.json"),
                &json!([{"id":"fixture-external-thread","cwd":root.join("project"),
                "name":"External conversation","createdAt":10000,"updatedAt":10000,"status":{"type":"notLoaded"},
                "historyMode":"paginated","path":rollout}]),
            )?;
        }
        "/list-fixture" | "/title-fixture" | "/list-fixture/reset" => {
            list_fixture(root, projects, path)?
        }
        "/paired-device" => pair_device(runtime, ticket, identity)?,
        "/import-candidates" => import_candidates(root)?,
        // A favicon the Host's icon discovery finds in the project's root,
        // which other scenarios' file lists and reviews never see.
        "/project-icon" => fs::write(
            root.join("project/favicon.svg"),
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><circle cx="8" cy="8" r="7" fill="#22c55e"/><path d="M4.5 8.5l2.5 2.5 4.5-5" stroke="#ffffff" stroke-width="2" fill="none"/></svg>"##,
        )?,
        "/background-task" => {
            let native = format!("fixture-background-{}", uuid::Uuid::new_v4());
            let path = root.join("list-fixture.json");
            let mut rows: Vec<serde_json::Value> = if path.exists() {
                serde_json::from_slice(&fs::read(&path)?)?
            } else {
                Vec::new()
            };
            rows.push(json!({
                "id":native,"cwd":root.join("project"),"name":"Simulator conversation",
                "createdAt":10000,"updatedAt":10000,"status":{"type":"notLoaded"},"turns":[]
            }));
            write_json(path, &rows)?;
            runtime.block_on(async {
                let connection =
                    Connection::open(ticket, Identity::from_bytes(identity.to_bytes())).await?;
                let _ = connection
                    .peer
                    .call(&agent_protocol::conversation::records::SubscribeList {
                        subscription: uuid::Uuid::new_v4(),
                        cached: None,
                        refresh: true,
                    })
                    .await?;
                connection.close().await;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
            })?;
            return Ok((
                200,
                serde_json::to_vec(&json!({"threadId":{"provider":"codex","id":native}}))?,
            ));
        }
        "/background-reply" => {
            fs::write(
                root.join("background-reply"),
                "Latest reply from another client",
            )?;
            let rollout = root.join("external-rollout.jsonl");
            if rollout.exists() {
                OpenOptions::new()
                    .append(true)
                    .open(rollout)?
                    .write_all(b"external reply persisted\n")?;
            }
        }
        "/client-reply" => runtime.block_on(async {
            let connection =
                Connection::open(ticket, Identity::from_bytes(identity.to_bytes())).await?;
            let result: Result<()> = async {
                use agent_protocol::{
                    conversation::{Command, InputContent, records},
                    session::{ProviderKind, SessionRef},
                };
                let target =
                    SessionRef::new(ProviderKind::Codex, "fixture-external-thread".into()).unwrap();
                connection
                    .peer
                    .call(&records::Subscribe {
                        session: target.clone(),
                        subscription: uuid::Uuid::new_v4(),
                        cached: None,
                        window: None,
                    })
                    .await?;
                let operation = uuid::Uuid::new_v4();
                let acknowledged = connection
                    .peer
                    .call(&records::Change {
                        session: target,
                        operation,
                        command: Command::Submit {
                            id: operation.to_string().into(),
                            content: InputContent {
                                author: None,
                                fresh_context: false,
                                parts: vec![agent_protocol::operations::Input::Text {
                                    text: "[success] Reply from another Bex client".into(),
                                }],
                                model: None,
                                effort: None,
                                service_tier: None,
                                follow_up: agent_protocol::operations::FollowUp::Queue,
                            },
                        },
                    })
                    .await?;
                acknowledged.accepted().map_err(|error| error.message)?;
                Ok(())
            }
            .await;
            connection.close().await;
            result
        })?,
        "/fail-next-history-read" => {
            fs::write(root.join("fail-next-history-read"), "")?;
        }
        "/fail-next-history-page" => {
            fs::write(root.join("fail-next-history-page"), "")?;
        }
        "/fail-next-fork" => {
            fs::write(root.join("fail-next-fork"), "")?;
        }
        "/hold-history-reads" => {
            fs::write(root.join("hold-history-reads"), [])?;
        }
        "/release-history-reads" => {
            for name in ["hold-history-reads", "history-read-held"] {
                match fs::remove_file(root.join(name)) {
                    Ok(()) => (),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                    Err(error) => return Err(error.into()),
                }
            }
        }
        _ => return Ok((404, Vec::new())),
    }
    Ok((204, Vec::new()))
}

// Native fixture item IDs locate a backing session, while UI assertions must
// use its persisted public address. Unmanaged fixture sessions own their ID.
fn session_identity(root: &Path, native: &str) -> Result<serde_json::Value> {
    use rusqlite::OptionalExtension;
    let path = root.join("bex-conversations.sqlite");
    let identity = if path.exists() {
        let connection = rusqlite::Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        connection.busy_timeout(Duration::from_secs(10))?;
        connection.query_row(
            "SELECT t.provider,t.id FROM sources s JOIN threads t ON t.key=s.thread WHERE s.provider='codex' AND s.native_id=?1",
            [native], |row| Ok((row.get::<_,Option<String>>(0)?,row.get::<_,String>(1)?)),
        ).optional()?
    } else {
        None
    };
    Ok(match identity {
        Some((provider, id)) => json!({"provider":provider,"id":id}),
        None => json!({"provider":"codex","id":native}),
    })
}

fn merge_worktree(root: &Path, path: &str) -> Result<()> {
    let repo = root.join("project/merge-repository");
    let checkout = root.join("project/merge-checkout");
    let git = |cwd: &Path, args: &[&str]| -> Result<()> {
        let output = std::process::Command::new("git")
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .output()?;
        if !output.status.success() {
            return Err("merge fixture Git command failed".into());
        }
        Ok(())
    };
    match path {
        "/merge-worktree/fresh" => {
            fs::create_dir(&repo)?;
            git(&repo, &["init", "-b", "main"])?;
            git(&repo, &["commit", "--allow-empty", "-m", "base"])?;
            git(
                &repo,
                &[
                    "worktree",
                    "add",
                    "-b",
                    "task",
                    checkout.to_str().ok_or("fixture path is not UTF-8")?,
                ],
            )?;
            git(
                &repo,
                &["config", "branch.task.bex-base-ref", "refs/heads/main"],
            )?;
        }
        "/merge-worktree/merged" => {
            git(&checkout, &["commit", "--allow-empty", "-m", "work"])?;
            git(&repo, &["merge", "--ff-only", "task"])?;
        }
        "/merge-worktree/dirty" => fs::write(checkout.join("pending.txt"), "pending\n")?,
        "/merge-worktree/clean" => fs::remove_file(checkout.join("pending.txt"))?,
        _ => {
            // The mark follows the same net file changes as Changes.
            fs::write(checkout.join("new-work.txt"), "new work\n")?;
            git(&checkout, &["add", "new-work.txt"])?;
            git(&checkout, &["commit", "-m", "new work"])?;
        }
    }
    write_json(
        root.join("list-fixture.json"),
        &json!([
            {"id":"merge-active","cwd":checkout,"name":"Merged running task","updatedAt":20000,"status":{"type":"active"}},
            {"id":"merge-idle","cwd":checkout,"name":"Merged idle task","updatedAt":19999,"status":{"type":"idle"}}
        ]),
    )
}

fn worktree_conversation(root: &Path, name: &str) -> Result<()> {
    // Bex creates session worktrees from a project repository and records
    // their origin; that record places the conversation in the project.
    let repository = root.join(format!("project/{name}-repository"));
    let worktree = root.join(format!("{name}-worktree"));
    fs::create_dir(&repository)?;
    let git = |arguments: &[&str]| -> Result<()> {
        let output = std::process::Command::new("git")
            .current_dir(&repository)
            .args(arguments)
            .output()?;
        if !output.status.success() {
            return Err("worktree fixture Git command failed".into());
        }
        Ok(())
    };
    git(&["init", "--initial-branch=main"])?;
    fs::write(repository.join("tracked.txt"), "original\n")?;
    git(&["add", "tracked.txt"])?;
    git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.test",
        "commit",
        "-m",
        "Initial",
    ])?;
    git(&[
        "worktree",
        "add",
        "-b",
        "session",
        worktree.to_str().ok_or("fixture path is not UTF-8")?,
    ])?;
    let records = root.join("bex-worktrees.json");
    let mut state: serde_json::Value = match fs::read(&records) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(error) => return Err(error.into()),
    };
    state["workspaceRoots"][worktree.to_str().ok_or("fixture path is not UTF-8")?] =
        json!(repository);
    write_json(records, &state)?;
    fs::write(repository.join("tracked.txt"), "Project-only change\n")?;
    fs::write(
        worktree.join("tracked.txt"),
        "Session worktree first\nSession worktree second\n",
    )?;
    write_json(
        root.join("list-fixture.json"),
        &json!([{
            "id":format!("fixture-{name}-thread"), "cwd":root.join("project"),
            "readCwd":worktree, "name":"Worktree conversation", "createdAt":10000,
            "updatedAt":10000,"status":{"type":"idle"}
        }]),
    )
}

#[cfg(unix)]
fn set_worktree_access(directory: &Path, readable: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if readable { 0o755 } else { 0o000 };
    fs::set_permissions(directory, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_worktree_access(_: &Path, _: bool) -> Result<()> {
    Err("directory access fixtures require Unix permissions".into())
}

fn set_upload_storage_available(root: &Path, available: bool) -> Result<()> {
    // Host repairs directory permissions before uploads. A regular file blocks
    // directory creation instead; keep earlier attachments intact for recovery.
    let directory = root.join("bex-attachments");
    let backup = root.join("bex-attachments-unavailable");
    if available {
        if backup.exists() {
            fs::remove_file(&directory)?;
            fs::rename(backup, directory)?;
        }
    } else if !backup.exists() {
        fs::create_dir_all(&directory)?;
        fs::rename(&directory, backup)?;
        fs::write(directory, [])?;
    }
    Ok(())
}

fn list_fixture(root: &Path, store: &host_daemon::ProjectStore, path: &str) -> Result<()> {
    let backup = root.join("projects-before-list-fixture.json");
    let fixture = root.join("list-fixture.json");
    if path.ends_with("/reset") {
        if backup.exists() {
            let projects: Vec<agent_protocol::models::Project> =
                serde_json::from_slice(&fs::read(&backup)?)?;
            store.replace_fixture_projects(&projects)?;
            fs::remove_file(backup)?;
        }
        match fs::remove_file(fixture) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        return Ok(());
    }
    if !backup.exists() {
        write_json(backup.clone(), &store.fixture_projects()?)?;
    }
    let mut projects = Vec::new();
    let mut threads = Vec::new();
    for number in 1..=16 {
        let id = format!("pagination-project-{number}");
        let cwd = root.join(&id);
        projects.push(agent_protocol::models::Project {
            id,
            name: format!("Project {number:02}"),
            roots: vec![agent_protocol::models::ProjectRoot {
                path: cwd.to_string_lossy().into_owned(),
            }],
            icon: None,
        });
        if path == "/title-fixture" {
            for conversation in 1..=16 {
                threads.push(json!({"id":format!("pagination-project-thread-{number}-{conversation}"),"cwd":cwd,
                    "name":format!("Project {number:02} conversation {conversation:02}"),"preview":"Unused preview. ".repeat(1000),"updatedAt":number * 100 + conversation}));
            }
        } else {
            threads.push(
                json!({"id":format!("pagination-project-thread-{number}"),"cwd":cwd,
                "name":format!("Project conversation {number:02}"),"updatedAt":number}),
            );
        }
    }
    let unassigned = root.join("unassigned");
    for number in 1..=16 {
        threads.push(json!({"id":format!("pagination-chat-{number}"),"cwd":unassigned,"name":format!("Chat {number:02}"),"updatedAt":100 + number}));
    }
    store.replace_fixture_projects(&projects)?;
    write_json(fixture, &threads)
}

/// A phone this Host allowed, named as the phone names itself.
fn pair_device(
    runtime: &tokio::runtime::Runtime,
    ticket: &Ticket,
    identity: &Identity,
) -> Result<()> {
    let invitation = rpc(runtime, ticket, identity, &CreateInvitation {})?;
    runtime.block_on(async {
        let device = Connection::open(ticket, Identity::generate()).await?;
        let paired: Result<()> = async {
            device
                .peer
                .call(&Pair {
                    invitation: invitation.invitation,
                })
                .await?;
            device
                .peer
                .call(&IdentifyDevice {
                    name: "検証 iPhone".into(),
                })
                .await?;
            Ok(())
        }
        .await;
        device.close().await;
        paired
    })
}

/// Codex history in a repository the Host has no project for, beside the
/// fixture's own project, for the project import to offer.
fn import_candidates(root: &Path) -> Result<()> {
    let repository = root.join("history-repository");
    fs::create_dir_all(repository.join(".git"))?;
    let mut threads = Vec::new();
    for (number, cwd) in [
        (1, &repository),
        (2, &repository),
        (3, &root.join("project")),
    ] {
        threads.push(json!({"id":format!("import-thread-{number}"),"cwd":cwd,
            "name":format!("Earlier conversation {number}"),"createdAt":number,"updatedAt":number}));
    }
    write_json(root.join("list-fixture.json"), &threads)
}

fn write_json(path: PathBuf, value: &impl serde::Serialize) -> Result<()> {
    agent_local_host::save_private_json(&path, value).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_controls_update_the_live_project_owner_and_restore_original_ids() {
        let root = tempfile::tempdir().unwrap();
        let store = host_daemon::HostRpcService::new([], root.path().join("bex-worktrees.json"))
            .fixture_projects();
        let original = vec![agent_protocol::models::Project {
            id: "original-project".into(),
            name: "Original".into(),
            roots: vec![agent_protocol::models::ProjectRoot {
                path: root.path().join("project").to_string_lossy().into_owned(),
            }],
            icon: None,
        }];
        store.replace_fixture_projects(&original).unwrap();
        let observer = store.clone();
        list_fixture(root.path(), &store, "/list-fixture").unwrap();
        let listed = observer.fixture_projects().unwrap();
        assert_eq!(listed.len(), 16);
        assert!(
            listed
                .iter()
                .any(|project| project.id == "pagination-project-16")
        );
        list_fixture(root.path(), &store, "/list-fixture/reset").unwrap();
        assert_eq!(observer.fixture_projects().unwrap(), original);
    }

    fn isolated_control_peer() -> (tokio::runtime::Runtime, Ticket, Identity) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let identity = Identity::generate();
        let node: iroh::EndpointId = identity.node_id().to_string().parse().unwrap();
        let ticket =
            iroh_tickets::endpoint::EndpointTicket::new(iroh::EndpointAddr::from_parts(node, []))
                .to_string()
                .parse()
                .unwrap();
        (runtime, ticket, identity)
    }

    #[test]
    fn history_scenarios_are_seeded_with_their_own_identity() {
        let (runtime, ticket, identity) = isolated_control_peer();
        let root = tempfile::tempdir().unwrap();
        for (path, id, kind) in [
            ("/long-conversation/scenario-a", "scenario-a", "long"),
            ("/long-conversation/scenario-b", "scenario-b", "long"),
            (
                "/viewport-conversation/scenario-c",
                "scenario-c",
                "viewport",
            ),
            ("/reading-conversation/scenario-d", "scenario-d", "reading"),
        ] {
            assert_eq!(
                route(
                    root.path(),
                    &host_daemon::HostRpcService::new([], root.path().join("bex-worktrees.json"))
                        .fixture_projects(),
                    &runtime,
                    &ticket,
                    &identity,
                    &Method::Post,
                    path
                )
                .unwrap()
                .0,
                204
            );
            let rows: serde_json::Value =
                serde_json::from_slice(&fs::read(root.path().join("list-fixture.json")).unwrap())
                    .unwrap();
            assert_eq!(rows.as_array().unwrap().len(), 1);
            assert_eq!(rows[0]["id"], id);
            assert_eq!(rows[0]["fixtureHistory"], kind);
        }
    }

    #[test]
    fn upload_storage_controls_fail_and_recover_without_touching_the_checkout() {
        // File controls must work without connecting to the fixture's RPC peer.
        let (runtime, ticket, identity) = isolated_control_peer();

        for existing in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let directory = root.path().join("bex-attachments");
            let checkout = root.path().join("project.txt");
            fs::write(&checkout, "draft stays available").unwrap();
            if existing {
                agent_local_host::create_state_directory(&directory).unwrap();
                fs::write(directory.join("earlier.png"), b"earlier photo").unwrap();
            }
            let control = |path| {
                assert_eq!(
                    route(
                        root.path(),
                        &host_daemon::HostRpcService::new(
                            [],
                            root.path().join("bex-worktrees.json")
                        )
                        .fixture_projects(),
                        &runtime,
                        &ticket,
                        &identity,
                        &Method::Post,
                        path
                    )
                    .unwrap()
                    .0,
                    204
                );
            };
            control("/uploads/restore");
            control("/uploads/unavailable");
            control("/uploads/unavailable");
            // Use the production upload preparation, including permission repair.
            assert!(agent_local_host::create_state_directory(&directory).is_err());
            assert!(fs::write(directory.join("selected.mov"), b"media").is_err());
            assert_eq!(
                fs::read_to_string(&checkout).unwrap(),
                "draft stays available"
            );
            control("/uploads/restore");
            control("/uploads/restore");
            agent_local_host::create_state_directory(&directory).unwrap();
            fs::write(directory.join("selected.mov"), b"media").unwrap();
            assert_eq!(fs::read(directory.join("selected.mov")).unwrap(), b"media");
            if existing {
                assert_eq!(
                    fs::read(directory.join("earlier.png")).unwrap(),
                    b"earlier photo"
                );
            }
            assert!(!root.path().join("bex-attachments-unavailable").exists());
        }
    }
}
