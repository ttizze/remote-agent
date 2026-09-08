//! Loopback-only UI test controls. No saved application profiles are used.

use crate::Result;
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    net::Ipv4Addr,
    os::unix::net::UnixStream,
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
    pub fn start(state: &Path, port: u16) -> Result<Self> {
        let state = state.canonicalize()?;
        let server = Server::http((Ipv4Addr::LOCALHOST, port))?;
        let port = server
            .server_addr()
            .to_ip()
            .ok_or("pairing server did not bind an IP socket")?
            .port();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                let Some(request) = server.recv_timeout(Duration::from_millis(100))? else {
                    continue;
                };
                let result = route(&state, request.method(), request.url());
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

fn rpc(state: &Path, target: &str, method: &str, params: Value) -> Result<Value> {
    let connection = UnixStream::connect(state.join("host.sock"))?;
    connection.set_read_timeout(Some(Duration::from_secs(10)))?;
    connection.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut stream = BufReader::new(connection);
    serde_json::to_writer(stream.get_mut(), &json!({"target":target}))?;
    stream.get_mut().write_all(b"\n")?;
    let mut line = String::new();
    stream.read_line(&mut line)?;
    if serde_json::from_str::<Value>(&line)? != json!({"ready":true}) {
        return Err("Host IPC did not become ready".into());
    }
    serde_json::to_writer(
        stream.get_mut(),
        &json!({"id":1,"method":method,"params":params}),
    )?;
    stream.get_mut().write_all(b"\n")?;
    loop {
        line.clear();
        if stream.read_line(&mut line)? == 0 {
            return Err("Host closed fixture IPC".into());
        }
        let mut response: Value = serde_json::from_str(&line)?;
        if response["id"] == 1 {
            return response
                .as_object_mut()
                .and_then(|response| response.remove("result"))
                .ok_or_else(|| "Host fixture request failed".into());
        }
    }
}

fn route(state: &Path, method: &Method, path: &str) -> Result<(u16, Vec<u8>)> {
    if method == &Method::Get && path == "/pairing" {
        return Ok((
            200,
            serde_json::to_vec(&rpc(state, "manager", "host/invite", json!({}))?)?,
        ));
    }
    if method != &Method::Post {
        return Ok((404, Vec::new()));
    }
    let root = state.parent().ok_or("fixture state has no parent")?;
    match path {
        "/worktree-conversation" => worktree_conversation(root)?,
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
                state,
                "local",
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
