//! Loopback-only UI test controls. No saved application profiles are used.

use crate::Result;
use crate::test_support::Connection;
use agent_core::transport::{Identity, Ticket};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
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
        let root = root.canonicalize()?;
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
                let response = Response::from_data(body)
                    .with_status_code(status)
                    .with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
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

fn rpc(
    runtime: &tokio::runtime::Runtime,
    ticket: &Ticket,
    identity: &Identity,
    method: &str,
    params: Value,
) -> Result<Value> {
    runtime.block_on(async {
        let connection =
            Connection::open(ticket, Identity::from_bytes(identity.to_bytes())).await?;
        let response = connection.peer.request::<_, Value>(method, &params).await;
        let closed = connection.close().await;
        let response = response?;
        closed?;
        Ok(response.value)
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
    if method == &Method::Get && path == "/pairing" {
        return Ok((
            200,
            serde_json::to_vec(&rpc(runtime, ticket, identity, "host/invite", json!({}))?)?,
        ));
    }
    if method != &Method::Post {
        return Ok((404, Vec::new()));
    }
    match path {
        "/auth-token/unavailable" => fs::write(root.join("auth-token-unavailable"), [])?,
        "/auth-token/reset" => {
            if root.join("auth-token-unavailable").exists() {
                fs::remove_file(root.join("auth-token-unavailable"))?;
            }
        }
        "/worktree-conversation" => worktree_conversation(root)?,
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
        "/long-conversation" => write_json(
            root.join("list-fixture.json"),
            &json!([{
            "id":"fixture-long-history","cwd":root.join("project"),"name":"Long interrupted conversation",
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
                "host/thread/start",
                json!({"cwd":root.join("project")}),
            )?;
            return Ok((
                200,
                serde_json::to_vec(&json!({"threadId":response["thread"]["id"]}))?,
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
        "/fail-next-history-read" => {
            fs::write(root.join("fail-next-history-read"), "")?;
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
    let projects_path = root.join("projects.json");
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
    let mut projects = serde_json::Map::new();
    let mut order = Vec::with_capacity(26);
    let mut threads = Vec::with_capacity(if path == "/title-fixture" { 508 } else { 66 });
    for number in 1..=26 {
        let id = format!("pagination-project-{number}");
        let cwd = root.join(&id);
        projects.insert(id.clone(), json!({"id":id,"name":format!("Project {number:02}"),"rootPaths":[cwd],"createdAt":1,"updatedAt":1}));
        order.push(id);
        if path == "/title-fixture" {
            for conversation in 1..=18 {
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
    for number in 1..=40 {
        threads.push(json!({"id":format!("pagination-chat-{number}"),"cwd":unassigned,"name":format!("Chat {number:02}"),"updatedAt":100 + number}));
    }
    write_json(
        projects_path,
        &json!({"local-projects":projects,"project-order":order}),
    )?;
    write_json(fixture, &threads)
}

fn write_json(path: PathBuf, value: &impl serde::Serialize) -> Result<()> {
    let mut file = std::io::BufWriter::new(fs::File::create(path)?);
    serde_json::to_writer(&mut file, value)?;
    file.flush()?;
    Ok(())
}
