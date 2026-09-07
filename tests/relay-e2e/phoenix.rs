use std::{path::Path, process::Stdio};
use host_protocol::RelayEndpoint;
use ring::rand::{SecureRandom, SystemRandom};
use tokio::{io::{AsyncBufReadExt, AsyncReadExt, BufReader}, process::{Child, Command}};

pub async fn start() -> (Child, RelayEndpoint) {
    let mut token = [0; 32];
    SystemRandom::new().fill(&mut token).unwrap();
    let token: String = token.iter().map(|byte| format!("{byte:02x}")).collect();
    let mut child = Command::new("mix")
        .args(["run", "--no-halt", "-e", "{:ok, {_, port}} = RemoteAgentServerWeb.Endpoint.server_info(:http); IO.puts(\"BEX_RELAY_PORT #{port}\")"])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/server"))
        .env("REMOTE_AGENT_RELAY_TOKEN", &token)
        .env("PHX_SERVER", "true").env("PHX_BIND_IP", "127.0.0.1").env("PORT", "0")
        .stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true)
        .spawn().expect("mix is required; use nix develop");
    let stderr = child.stderr.take().unwrap();
    let error_log = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await.unwrap();
        bytes
    });
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
    let port = loop {
        let Some(line) = output.next_line().await.unwrap() else {
            let status = child.wait().await.unwrap();
            let errors = error_log.await.unwrap();
            panic!("Phoenix exited {status}: {}", String::from_utf8_lossy(&errors));
        };
        if let Some(port) = line.strip_prefix("BEX_RELAY_PORT ") {
            break port.parse::<u16>().unwrap();
        }
    };
    tokio::spawn(async move { while output.next_line().await.ok().flatten().is_some() {} });
    // The child owns the pipe. Kill-on-drop closes it and lets the log reader
    // finish even when an assertion or the outer timeout fails.
    (child, RelayEndpoint { relay_url: format!("ws://127.0.0.1:{port}/socket/websocket"), relay_token: token, runner_id: "one".into() })
}

