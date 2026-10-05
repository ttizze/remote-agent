//! Loopback-only UI test controls. No saved application profiles are used.

use crate::{Result, test_support::Connection};
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

pub struct PairingServer {
    pub port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<()>>>,
}

impl PairingServer {
    pub fn start(root: &Path, ticket: Ticket, identity: Identity) -> Result<Self> {
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
                let result = route(
                    &root,
                    &runtime,
                    &ticket,
                    &identity,
                    request.method(),
                    request.url(),
                );
                let (status, body) = match result {
                    Ok(response) => response,
                    Err(_) => (503, b"Isolated Host is unavailable".to_vec()),
                };
                let content_type = if request.url() == "/browser-test" {
                    "text/html; charset=utf-8"
                } else {
                    "application/json"
                };
                let response = Response::from_data(body)
                    .with_status_code(status)
                    .with_header(Header::from_bytes("Content-Type", content_type).unwrap())
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

fn route(
    root: &Path,
    runtime: &tokio::runtime::Runtime,
    ticket: &Ticket,
    identity: &Identity,
    method: &Method,
    path: &str,
) -> Result<(u16, Vec<u8>)> {
    if method == &Method::Get && path == "/browser-test" {
        return Ok((200, br#"<title>BEX browser fixture</title><body style="margin:0"><input style="position:absolute;left:20px;top:20px;width:300px;height:40px" oninput="document.title=this.value"><p style="position:absolute;top:100px">Shared browser on the Host</p>"#.to_vec()));
    }
    if method == &Method::Get && path == "/pairing" {
        return Ok((
            200,
            serde_json::to_vec(&rpc(
                runtime,
                ticket,
                identity,
                &agent_core::state::operations::CreateInvitation {},
            )?)?,
        ));
    }
    if method != &Method::Post {
        return Ok((404, Vec::new()));
    }
    // This control translates one exact fixture source; production never exposes native IDs.
    if let Some(native) = path.strip_prefix("/conversation/") {
        return Ok((200, serde_json::to_vec(&conversation(root, native)?)?));
    }
    match path {
        "/auth-token/unavailable" => fs::write(root.join("auth-token-unavailable"), [])?,
        "/auth-token/reset" => {
            if root.join("auth-token-unavailable").exists() {
                fs::remove_file(root.join("auth-token-unavailable"))?;
            }
        }
        "/worktree-conversation" => worktree_conversation(root)?,
        "/merge-worktree/fresh"
        | "/merge-worktree/merged"
        | "/merge-worktree/new-work"
        | "/merge-worktree/dirty"
        | "/merge-worktree/clean" => merge_worktree(root, path)?,
        "/worktree/unavailable" => fs::rename(
            root.join("review-worktree"),
            root.join("review-worktree-unavailable"),
        )?,
        "/worktree/restore" => fs::rename(
            root.join("review-worktree-unavailable"),
            root.join("review-worktree"),
        )?,
        "/completed-history" => write_json(
            root.join("list-fixture.json"),
            &json!([{"id":"fixture-thread-persisted","cwd":root.join("project"),
            "name":"Persisted completed history","createdAt":10000,"updatedAt":10000,
            "status":{"type":"notLoaded"},"historyMode":"paginated",
            "turns":[{"id":"fixture-turn-persisted","status":"completed","items":[
                {"id":"fixture-command-persisted","type":"commandExecution","command":"./gradlew test",
                 "status":"completed","aggregatedOutput":crate::fixture::history::detail_output(),"exitCode":0},
                {"id":"fixture-final-persisted","type":"agentMessage","phase":"final_answer","text":"Persisted history complete."}
            ]}]}]),
        )?,
        "/repeated-history" => write_json(
            root.join("list-fixture.json"),
            &json!([
                {"id":"fixture-repeated-history","name":"Repeated history","cwd":root.join("project"), "historyMode":"paginated",
                 "turns":[{"id":"repeated","status":"completed","items":[{"id":"duplicate-history-old","type":"agentMessage","phase":"final_answer","text":"Older AI response must remain visible."}]},
                          {"id":"repeated","status":"completed","items":[{"id":"duplicate-history-new","type":"agentMessage","phase":"final_answer","text":"Newer AI response must remain visible."}]}]}
            ]),
        )?,
        "/long-conversation" | "/viewport-conversation" => write_json(
            root.join("list-fixture.json"),
            &json!([{
            "id":if path == "/viewport-conversation" { "fixture-viewport-history" } else { "fixture-long-history" },
            "cwd":root.join("project"),"name":"Long conversation",
            "createdAt":10000,"updatedAt":10000,"status":{"type":"notLoaded"},"historyMode":"paginated"}]),
        )?,
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
        "/list-fixture" | "/title-fixture" | "/list-fixture/reset" => list_fixture(root, path)?,
        "/background-task" => {
            let response = rpc(
                runtime,
                ticket,
                identity,
                &agent_core::state::operations::CreateSession {
                    instance_id: "codex"
                        .parse::<agent_protocol::session::ProviderInstanceId>()
                        .unwrap(),
                    cwd: Some(root.join("project").to_string_lossy().into_owned()),
                    model: None,
                },
            )?;
            return Ok((
                200,
                serde_json::to_vec(&json!({"threadId":response.response.thread.id}))?,
            ));
        }
        "/background-reply" => {
            let db = rusqlite::Connection::open_with_flags(
                root.join("bex-conversations.sqlite"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            db.busy_timeout(Duration::from_secs(5))?;
            let id: String = db.query_row("SELECT id FROM conversations WHERE provider=?1 ORDER BY json_extract(metadata, '$.updatedAt') DESC, rowid DESC LIMIT 1", ["\"codex\""], |row| row.get(0))?;
            rpc(
                runtime,
                ticket,
                identity,
                &agent_protocol::operations::Submission {
                    thread_id: agent_protocol::session::SessionRef { id },
                    client_user_message_id: format!("other-client-{}", uuid::Uuid::new_v4()).into(),
                    input: vec![agent_protocol::operations::Input::Text {
                        text: "[external-reply] Latest reply from another client".into(),
                    }],
                    model: None,
                    options: Vec::new(),
                },
            )?;
        }
        "/client-reply" => runtime.block_on(async {
            let connection =
                Connection::open(ticket, Identity::from_bytes(identity.to_bytes())).await?;
            let target = conversation(root, "fixture-external-thread")?;
            let result: Result<()> = async {
                connection
                    .peer
                    .request::<agent_protocol::session::OpenedSession>(
                        &agent_protocol::protocol::Call::OpenSession(
                            serde_json::from_value::<agent_protocol::session::OpenSession>(
                                json!({"session":target,"limit":5}),
                            )
                            .unwrap(),
                        ),
                    )
                    .await
                    .map(|output| serde_json::to_value(output).unwrap())?;
                connection
                    .peer
                    .call(
                        &serde_json::from_value::<agent_protocol::operations::Submission>(
                            json!({"threadId":target,"clientUserMessageId":"fixture-other-client",
                        "input":[{"text":{"text":"[success] Reply from another Bex client"}}]}),
                        )
                        .unwrap(),
                    )
                    .await
                    .map(|output| serde_json::to_value(output).unwrap())?;
                Ok(())
            }
            .await;
            connection.close().await;
            result
        })?,
        "/fail-next-thread-start" => {
            fs::write(root.join("fail-next-thread-start"), "")?;
        }
        "/fail-history-read" => {
            let db = rusqlite::Connection::open(root.join("bex-conversations.sqlite"))?;
            db.busy_timeout(Duration::from_secs(5))?;
            let saved: (String, String) = db.query_row("SELECT id, metadata FROM conversations ORDER BY json_extract(metadata, '$.updatedAt') DESC, rowid DESC LIMIT 1", [], |row| Ok((row.get(0)?, row.get(1)?)))?;
            write_json(
                root.join("failed-history.json"),
                &serde_json::to_value(&saved)?,
            )?;
            // A valid JSON projection with a damaged identity fails only the
            // detail read, while title/catalog requests remain available.
            db.execute(
                "UPDATE conversations SET metadata=json_set(metadata, '$.id', NULL) WHERE id=?1",
                [&saved.0],
            )?;
        }
        "/restore-history-read" => {
            let saved = root.join("failed-history.json");
            if saved.exists() {
                let (id, metadata): (String, String) = serde_json::from_slice(&fs::read(&saved)?)?;
                let db = rusqlite::Connection::open(root.join("bex-conversations.sqlite"))?;
                db.busy_timeout(Duration::from_secs(5))?;
                db.execute(
                    "UPDATE conversations SET metadata=?2 WHERE id=?1",
                    rusqlite::params![id, metadata],
                )?;
                fs::remove_file(saved)?;
            }
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
    if matches!(
        path,
        "/completed-history"
            | "/repeated-history"
            | "/long-conversation"
            | "/viewport-conversation"
            | "/external-conversation"
            | "/list-fixture"
            | "/title-fixture"
            | "/list-fixture/reset"
            | "/merge-worktree/fresh"
            | "/merge-worktree/merged"
            | "/merge-worktree/new-work"
            | "/merge-worktree/dirty"
            | "/merge-worktree/clean"
            | "/worktree-conversation"
    ) {
        rpc(
            runtime,
            ticket,
            identity,
            &agent_core::state::operations::ImportHistory {},
        )?;
    }
    Ok((204, Vec::new()))
}

fn conversation(root: &Path, native: &str) -> Result<agent_protocol::session::SessionRef> {
    let db = rusqlite::Connection::open_with_flags(
        root.join("bex-conversations.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    db.busy_timeout(Duration::from_secs(5))?;
    let id = db.query_row(
        "SELECT id FROM conversations WHERE provider=?1 AND native_id=?2",
        rusqlite::params!["\"codex\"", native],
        |row| row.get(0),
    )?;
    Ok(agent_protocol::session::SessionRef { id })
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
        }
        "/merge-worktree/merged" => {
            git(&checkout, &["commit", "--allow-empty", "-m", "work"])?;
            git(&repo, &["merge", "--ff-only", "task"])?;
        }
        "/merge-worktree/dirty" => fs::write(checkout.join("pending.txt"), "pending\n")?,
        "/merge-worktree/clean" => fs::remove_file(checkout.join("pending.txt"))?,
        _ => {
            // Unmerged status tracks net file changes, not empty commits.
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

fn worktree_conversation(root: &Path) -> Result<()> {
    let repository = root.join("review-repository");
    let worktree = root.join("review-worktree");
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
    fs::write(repository.join("tracked.txt"), "Project-only change\n")?;
    fs::write(
        worktree.join("tracked.txt"),
        "Session worktree first\nSession worktree second\n",
    )?;
    write_json(
        root.join("list-fixture.json"),
        &json!([{
            "id":"fixture-worktree-thread", "cwd":root.join("project"),
            "readCwd":worktree, "name":"Worktree conversation", "createdAt":10000,
            "updatedAt":10000,"status":{"type":"idle"}
        }]),
    )
}

fn list_fixture(root: &Path, path: &str) -> Result<()> {
    let projects_path = root.join("bex-projects.json");
    let backup = root.join("projects-before-list-fixture.json");
    let fixture = root.join("list-fixture.json");
    if path.ends_with("/reset") {
        if backup.exists() {
            fs::copy(&backup, projects_path)?;
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
        fs::copy(&projects_path, &backup)?;
    }
    let mut projects = Vec::new();
    let mut threads = Vec::new();
    for number in 1..=16 {
        let id = format!("pagination-project-{number}");
        let cwd = root.join(&id);
        projects
            .push(json!({"id":id,"name":format!("Project {number:02}"),"roots":[{"path":cwd}]}));
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
    write_json(projects_path, &projects)?;
    write_json(fixture, &threads)
}

fn write_json(path: PathBuf, value: &impl serde::Serialize) -> Result<()> {
    let mut file = std::io::BufWriter::new(fs::File::create(path)?);
    serde_json::to_writer(&mut file, value)?;
    file.flush()?;
    Ok(())
}
