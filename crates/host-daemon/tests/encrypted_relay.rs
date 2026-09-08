use std::{path::Path, sync::{Arc, Mutex as StdMutex, atomic::{AtomicBool, Ordering}}, time::{Duration, SystemTime, UNIX_EPOCH}};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use codex_app_server::{AppServerConfig, CodexAppServer};
use futures_util::{SinkExt, StreamExt};
use host_daemon::{CodexRpcService, DesktopProjectStore, DeviceAuthenticationState, EncryptedGateway, HostIdentity};
use host_protocol::{Ed25519PublicKey, PairingToken, RelayEndpoint};
use mobile_client::{MobileClient, MobileClientConfig, MobileClientError};
use ring::{rand::SystemRandom, signature::{Ed25519KeyPair, KeyPair}};
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::Mutex, task::JoinSet};
use tokio_tungstenite::{accept_hdr_async, connect_async, tungstenite::{Message, handshake::server::{Request, Response}}};

#[path = "../../../tests/relay-e2e/phoenix.rs"]
mod phoenix;

fn key() -> Vec<u8> { Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap().as_ref().to_vec() }
fn public_key(key: &[u8]) -> Ed25519PublicKey {
    Ed25519PublicKey::from_bytes(Ed25519KeyPair::from_pkcs8(key).unwrap().public_key().as_ref().try_into().unwrap())
}
fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64 }
fn config(relay: &RelayEndpoint, host: Ed25519PublicKey, ticket: Option<PairingToken>) -> MobileClientConfig {
    MobileClientConfig { relay: relay.clone(), host_identity: host, device_name: "isolated iPhone".into(), pairing_ticket: ticket, request_timeout: Duration::from_secs(5) }
}

/// The proxy can read and replace the same decoded Phoenix payloads the relay
/// operator can see. WebSocket masking is removed before confidentiality checks.
async fn observing_proxy(endpoint: &RelayEndpoint, captured: Arc<StdMutex<Vec<u8>>>, tamper: Arc<AtomicBool>) -> (RelayEndpoint, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut client_endpoint = endpoint.clone();
    client_endpoint.relay_url = format!("ws://{}/socket/websocket", listener.local_addr().unwrap());
    let actual = endpoint.socket_url("mobile").unwrap().to_string();
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let captured = captured.clone();
            let tamper = tamper.clone();
            let actual = actual.clone();
            connections.spawn(async move {
                let mut downstream = accept_hdr_async(stream, |_request: &Request, response: Response| Ok(response)).await.unwrap();
                let (mut upstream, _) = connect_async(actual).await.unwrap();
                loop {
                    tokio::select! {
                        message = downstream.next() => {
                            let Some(Ok(message)) = message else { return };
                            let message = observe(message, &captured, Some(&tamper));
                            if upstream.send(message).await.is_err() { return; }
                        }
                        message = upstream.next() => {
                            let Some(Ok(message)) = message else { return };
                            let message = observe(message, &captured, None);
                            if downstream.send(message).await.is_err() { return; }
                        }
                    }
                }
            });
            while connections.try_join_next().is_some() {}
        }
    });
    (client_endpoint, task)
}

fn observe(message: Message, captured: &StdMutex<Vec<u8>>, tamper: Option<&AtomicBool>) -> Message {
    if let Message::Text(text) = &message {
        let mut frame: Value = serde_json::from_str(text).unwrap();
        if frame[3] == "data" {
            let mut bytes = STANDARD.decode(frame[4]["data"].as_str().unwrap()).unwrap();
            let mut capture = captured.lock().unwrap();
            assert!(capture.len() + bytes.len() < 2 * 1024 * 1024, "bounded test capture exceeded");
            capture.extend_from_slice(&bytes);
            if tamper.is_some_and(|flag| flag.swap(false, Ordering::SeqCst)) {
                let last = bytes.last_mut().unwrap();
                *last ^= 1;
                frame[4]["data"] = Value::String(STANDARD.encode(bytes));
                return Message::text(frame.to_string());
            }
        }
    }
    message
}

fn fixture_program(directory: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/fixtures/fake-codex-app-server.py").canonicalize().unwrap();
    let program = directory.join("codex-fixture.py");
    let source = format!("#!/usr/bin/env python3\nimport os,runpy\nos.environ.pop('BEX_FAKE_CODEX_TRACE', None)\nos.environ.pop('BEX_FAKE_CODEX_EXPECTED_CWD', None)\nos.environ['CODEX_HOME'] = {}\nrunpy.run_path({}, run_name='__main__')\n", serde_json::to_string(&directory.to_string_lossy()).unwrap(), serde_json::to_string(&fixture.to_string_lossy()).unwrap());
    std::fs::write(&program, source).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    program
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn encrypted_relay_authenticates_devices_isolates_ids_rejects_tampering_and_revokes_access() {
    tokio::time::timeout(Duration::from_secs(90), async {
        let directory = tempfile::tempdir().unwrap();
        let (mut relay_server, endpoint) = phoenix::start().await;
        let host_key = key();
        let host = HostIdentity::from_pkcs8(&host_key).unwrap();
        let host_public = host.public_key();
        let authentication = Arc::new(Mutex::new(DeviceAuthenticationState::load(directory.path().join("devices.json")).unwrap()));
        let server = Arc::new(CodexAppServer::spawn(AppServerConfig { program: fixture_program(directory.path()), request_timeout: Duration::from_secs(5), ..AppServerConfig::default() }).await.unwrap());
        let service = CodexRpcService::new(server.clone(), DesktopProjectStore::new(directory.path().join("projects.json")));
        service.start();
        let gateway = Arc::new(EncryptedGateway::new(&host, authentication.clone(), service.clone()));
        let (mut connections, runner) = relay_transport::connect_runner(&endpoint).await.unwrap();
        let serving_gateway = gateway.clone();
        let serving = tokio::spawn(async move {
            let mut sessions = JoinSet::new();
            while let Some(connection) = connections.recv().await {
                let gateway = serving_gateway.clone();
                sessions.spawn(async move { let _ = gateway.serve(connection).await; });
                while sessions.try_join_next().is_some() {}
            }
            while sessions.join_next().await.is_some() {}
        });

        let first_key = key();
        let first_ticket = authentication.lock().await.invite(host_public, "Mac".into(), endpoint.clone(), now()).unwrap().ticket;
        assert!(MobileClient::connect(config(&endpoint, Ed25519PublicKey::from_bytes([8; 32]), Some(first_ticket)), &first_key).await.is_err(), "wrong Host pin must fail before pairing");
        assert!(authentication.lock().await.devices().is_empty());
        let expired = authentication.lock().await.invite(host_public, "Mac".into(), endpoint.clone(), 0).unwrap().ticket;
        assert!(matches!(MobileClient::connect(config(&endpoint, host_public, Some(expired)), &first_key).await, Err(MobileClientError::AuthenticationRejected)));
        assert!(matches!(MobileClient::connect(config(&endpoint, host_public, None), &first_key).await, Err(MobileClientError::AuthenticationRejected)));

        let capture = Arc::new(StdMutex::new(Vec::new()));
        let tamper = Arc::new(AtomicBool::new(false));
        let (observed_endpoint, proxy) = observing_proxy(&endpoint, capture.clone(), tamper.clone()).await;
        let first = MobileClient::connect(config(&observed_endpoint, host_public, Some(first_ticket)), &first_key).await.unwrap();
        assert!(matches!(MobileClient::connect(config(&endpoint, host_public, Some(first_ticket)), &first_key).await, Err(MobileClientError::AuthenticationRejected)), "invitation replay must fail");
        let second_key = key();
        let second_ticket = authentication.lock().await.invite(host_public, "Mac".into(), endpoint.clone(), now()).unwrap().ticket;
        let second = MobileClient::connect(config(&endpoint, host_public, Some(second_ticket)), &second_key).await.unwrap();
        let first_cwd = directory.path().join("private-alpha-7da76fbedf25447b");
        let second_cwd = directory.path().join("private-bravo-c1a36dd641944c23");
        // Both clients issue request ID 1; neither response may cross routes.
        let (a, b) = tokio::join!(first.request("thread/start", json!({"cwd": first_cwd})), second.request("thread/start", json!({"cwd": second_cwd})));
        let a = a.unwrap();
        let b = b.unwrap();
        assert_eq!(a["thread"]["cwd"], first_cwd.to_string_lossy().as_ref());
        assert_eq!(b["thread"]["cwd"], second_cwd.to_string_lossy().as_ref());
        assert_ne!(a["thread"]["id"], b["thread"]["id"]);
        let capture = capture.lock().unwrap();
        assert!(capture.len() > 1000);
        for plaintext in ["private-alpha-7da76fbedf25447b", "thread/start", "fixture-thread"] {
            assert!(!capture.windows(plaintext.len()).any(|bytes| bytes == plaintext.as_bytes()), "relay could read application content");
        }
        drop(capture);

        let mut messages = first.subscribe();
        let thread_id = &a["thread"]["id"];
        let started = first.request("turn/start", json!({
            "threadId":thread_id,"clientUserMessageId":"first-client-message",
            "input":[{"type":"text","text":"[delayed-input] Begin"}]
        })).await.unwrap();
        first.request("turn/steer", json!({
            "threadId":thread_id,"expectedTurnId":started["turn"]["id"],
            "clientUserMessageId":"steering-client-message","input":[{"type":"text","text":"Follow through"}]
        })).await.unwrap();
        std::fs::write(directory.path().join("release-inputs"), "release").unwrap();
        let ids = tokio::time::timeout(Duration::from_secs(10), async {
            let mut ids = std::collections::HashSet::new();
            while ids.len() < 2 {
                let event: Value = serde_json::from_str(&messages.recv().await.unwrap()).unwrap();
                if event["method"] == "item/started" && event["params"]["item"]["type"] == "userMessage" {
                    ids.insert(event["params"]["item"]["clientId"].as_str().unwrap().to_owned());
                }
            }
            ids
        }).await.expect("accepted inputs must retain their client IDs across the encrypted native event stream");
        assert_eq!(ids, ["first-client-message".to_owned(), "steering-client-message".to_owned()].into());
        let history = first.request("host/thread/read", json!({"threadId":thread_id,"includeTurns":true})).await.unwrap();
        let user_ids = history["thread"]["turns"][0]["items"].as_array().unwrap().iter()
            .filter(|item| item["type"] == "userMessage").map(|item| item["clientId"].as_str().unwrap()).collect::<Vec<_>>();
        assert_eq!(user_ids.len(), 2);

        tamper.store(true, Ordering::SeqCst);
        assert!(first.request("thread/start", json!({"cwd": directory.path().join("tampered")})).await.is_err(), "SSH must reject modified ciphertext");
        assert_eq!(second.request("thread/list", json!({})).await.unwrap()["data"].as_array().unwrap().len(), 2, "tampered command reached Codex");
        first.close();
        let reconnected = MobileClient::connect(config(&endpoint, host_public, None), &first_key).await.unwrap();
        let mut closed = reconnected.subscribe();
        authentication.lock().await.revoke(public_key(&first_key)).unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(5), closed.recv()).await.unwrap().is_err());
        assert!(reconnected.request("thread/list", json!({})).await.is_err());
        assert_eq!(second.request("thread/list", json!({})).await.unwrap()["data"].as_array().unwrap().len(), 2);
        assert!(matches!(MobileClient::connect(config(&endpoint, host_public, None), &first_key).await, Err(MobileClientError::AuthenticationRejected)));

        reconnected.close();
        second.close();
        drop(runner);
        serving.await.unwrap();
        proxy.abort();
        let _ = proxy.await;
        drop(gateway);
        drop(service);
        Arc::try_unwrap(server).ok().expect("SSH tasks retained the Codex process").shutdown().await.unwrap();
        relay_server.kill().await.unwrap();
        relay_server.wait().await.unwrap();
    }).await.expect("encrypted full-stack loop exceeded its deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_management_and_relay_share_one_codex_and_shutdown_releases_sessions() {
    use host_daemon::{HostRuntime, LocalListener};
    use host_protocol::{JsonlReader, JsonlWriter, PairingQrPayload};
    use tokio::{io::{AsyncWriteExt, split}, net::UnixStream};
    use tokio_util::sync::CancellationToken;
    use std::os::unix::fs::PermissionsExt;
    tokio::time::timeout(Duration::from_secs(45), async {
        let directory = tempfile::tempdir().unwrap();
        let state = directory.path().join("state");
        let listener = LocalListener::bind(&state).unwrap();
        assert!(LocalListener::bind(&state).is_err(), "a second daemon must not replace the active socket");
        assert_eq!(std::fs::metadata(state.join("host.sock")).unwrap().permissions().mode() & 0o777, 0o600);
        let (mut relay_server, endpoint) = phoenix::start().await;
        let identity = HostIdentity::from_pkcs8(&key()).unwrap();
        let server = Arc::new(CodexAppServer::spawn(AppServerConfig { program: fixture_program(directory.path()), request_timeout: Duration::from_secs(5), ..AppServerConfig::default() }).await.unwrap());
        let service = CodexRpcService::new(server.clone(), DesktopProjectStore::new(directory.path().join("projects.json")));
        service.start();
        let runtime = Arc::new(HostRuntime::new(service, identity, DeviceAuthenticationState::load(state.join("devices.json")).unwrap(), "Fixture Mac".into(), endpoint.clone(), host_daemon::RemoteHosts::load(Arc::new(MemoryCredentials::default())).unwrap()).unwrap());
        let stop = CancellationToken::new();
        let serving = tokio::spawn(runtime.clone().run(listener, stop.clone()));
        let mut manager = UnixStream::connect(state.join("host.sock")).await.unwrap();
        manager.write_all(b"{\"target\":\"manager\"}\n").await.unwrap();
        let (read, write) = split(manager);
        let mut reader = JsonlReader::new(read);
        let mut writer = JsonlWriter::new(write);
        assert_eq!(reader.read_line().await.unwrap().unwrap(), "{\"ready\":true}");
        loop {
            writer.write_line(r#"{"id":1,"method":"host/status"}"#).await.unwrap();
            let status: Value = serde_json::from_str(&reader.read_line().await.unwrap().unwrap()).unwrap();
            if status["result"]["relayConnected"] == true { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        writer.write_line(r#"{"id":2,"method":"host/invite"}"#).await.unwrap();
        let invitation: Value = serde_json::from_str(&reader.read_line().await.unwrap().unwrap()).unwrap();
        let invitation: PairingQrPayload = serde_json::from_value(invitation["result"].clone()).unwrap();
        let device_key = key();
        let mobile = MobileClient::connect(config(&endpoint, invitation.host_identity, Some(invitation.ticket)), &device_key).await.unwrap();
        // A recording longer than the former phone limit must reach Host
        // dictation, never the Codex request parser. This fixture has no account.
        let audio = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, vec![0_u8; 24_000 * 2 * 31]);
        let error = mobile.request("host/dictation/transcribe", json!({"audio":audio})).await.unwrap_err().to_string();
        assert!(error.contains("dictation_failed"), "dictation escaped the Host route: {error}");
        assert!(error.contains("ChatGPT"), "long recording did not reach authentication: {error}");
        let document = directory.path().join("document.txt");
        std::fs::write(&document, b"\xef\xbb\xbfalpha\r\nbeta\r\n").unwrap();
        std::fs::set_permissions(&document, std::fs::Permissions::from_mode(0o640)).unwrap();
        let read = mobile.request("host/file/read", json!({"path":document})).await.unwrap();
        assert_eq!(read["bom"], true);
        assert_eq!(read["lineEnding"], "crlf");
        let saved = mobile.request("host/file/write", json!({"path":document,"revision":read["revision"],"text":"changed\n日本語\n"})).await.unwrap();
        assert_ne!(read["revision"], saved["revision"]);
        assert_eq!(std::fs::read(&document).unwrap(), "\u{feff}changed\r\n日本語\r\n".as_bytes());
        assert_eq!(std::fs::metadata(&document).unwrap().permissions().mode() & 0o777, 0o640);
        assert!(mobile.request("host/file/write", json!({"path":document,"revision":read["revision"],"text":"stale overwrite"})).await.is_err());
        assert_eq!(mobile.request("host/file/read", json!({"path":document})).await.unwrap()["text"], "changed\r\n日本語\r\n");
        let binary = directory.path().join("binary.dat");
        let data: Vec<u8> = (0..4*1024*1024).map(|index| (index % 251) as u8).collect();
        std::fs::write(&binary, &data).unwrap();
        let upload = mobile.upload_file(&binary, directory.path(), "写真.dat").await.unwrap();
        let uploaded = Path::new(upload["path"].as_str().unwrap());
        assert_eq!(std::fs::read(uploaded).unwrap(), data);
        let download = directory.path().join("download.dat");
        mobile.download_file(uploaded, &download).await.unwrap();
        assert_eq!(std::fs::read(&download).unwrap(), data);
        assert!(mobile.download_file(uploaded, &download).await.is_err(), "download must preserve an existing destination");
        assert_eq!(std::fs::read(&download).unwrap(), data);
        let mut local = UnixStream::connect(state.join("host.sock")).await.unwrap();
        // One write deliberately pipelines header + request to detect lost
        // read-ahead bytes when the manager selects the Codex socket target.
        local.write_all(b"{\"target\":\"local\"}\n{\"id\":7,\"method\":\"thread/start\",\"params\":{\"cwd\":\"/isolated-local\"}}\n").await.unwrap();
        let mut local = JsonlReader::new(local);
        assert_eq!(local.read_line().await.unwrap().unwrap(), "{\"ready\":true}");
        loop {
            let line: Value = serde_json::from_str(&local.read_line().await.unwrap().unwrap()).unwrap();
            if line["id"] == 7 { assert_eq!(line["result"]["thread"]["cwd"], "/isolated-local"); break; }
        }
        drop(local);
        let threads = mobile.request("thread/list", json!({})).await.unwrap();
        assert_eq!(threads["data"].as_array().unwrap().len(), 1, "closing local UI must preserve shared Codex state");
        assert!(mobile.request("host/invite", json!({})).await.is_err(), "remote RPC cannot reach local management");
        writer.write_line(&json!({"id":3,"method":"host/revoke","params":{"identity":public_key(&device_key)}}).to_string()).await.unwrap();
        let revoked: Value = serde_json::from_str(&reader.read_line().await.unwrap().unwrap()).unwrap();
        assert!(revoked.get("result").is_some());
        assert!(mobile.request("thread/list", json!({})).await.is_err());
        writer.write_line(r#"{"id":4,"method":"host/invite"}"#).await.unwrap();
        let invitation: Value = serde_json::from_str(&reader.read_line().await.unwrap().unwrap()).unwrap();
        writer.write_line(&json!({"id":5,"method":"host/pairRemote","params":{"invitation":invitation["result"],"deviceName":"Other Mac"}}).to_string()).await.unwrap();
        let paired: Value = serde_json::from_str(&reader.read_line().await.unwrap().unwrap()).unwrap();
        let profile = paired["result"]["id"].as_str().expect("remote pairing failed");
        assert!(paired.to_string().find("relayToken").is_none(), "UI profile exposed relay credentials");
        let mut remote = UnixStream::connect(state.join("host.sock")).await.unwrap();
        remote.write_all(format!("{}\n{}\n", json!({"target":"remote","profileId":profile}), json!({"id":"remote-opaque-id","method":"thread/list","params":{}})).as_bytes()).await.unwrap();
        let mut remote = JsonlReader::new(remote);
        assert_eq!(remote.read_line().await.unwrap().unwrap(), "{\"ready\":true}");
        loop {
            let response: Value = serde_json::from_str(&remote.read_line().await.unwrap().unwrap()).unwrap();
            if response["id"] == "remote-opaque-id" { assert_eq!(response["result"]["data"].as_array().unwrap().len(), 1); break; }
        }
        writer.write_line(&json!({"id":6,"method":"host/removeRemote","params":{"id":profile}}).to_string()).await.unwrap();
        assert!(serde_json::from_str::<Value>(&reader.read_line().await.unwrap().unwrap()).unwrap().get("result").is_some());
        writer.write_line(r#"{"id":7,"method":"host/listRemotes"}"#).await.unwrap();
        assert_eq!(serde_json::from_str::<Value>(&reader.read_line().await.unwrap().unwrap()).unwrap()["result"], json!([]));
        stop.cancel();
        serving.await.unwrap().unwrap();
        drop(runtime);
        assert!(!state.join("host.sock").exists());
        Arc::try_unwrap(server).ok().expect("runtime retained Codex after shutdown").shutdown().await.unwrap();
        let replacement = LocalListener::bind(&state).unwrap();
        drop(replacement);
        relay_server.kill().await.unwrap();
        relay_server.wait().await.unwrap();
    }).await.expect("local/relay runtime exceeded its deadline");
}


#[derive(Default)]
struct MemoryCredentials(StdMutex<Option<zeroize::Zeroizing<Vec<u8>>>>);
impl host_daemon::RemoteCredentialStore for MemoryCredentials {
    fn load(&self) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>, String> { Ok(self.0.lock().unwrap().clone()) }
    fn save(&self, bytes: &[u8]) -> Result<(), String> { *self.0.lock().unwrap() = Some(zeroize::Zeroizing::new(bytes.to_vec())); Ok(()) }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn large_history_loads_conversation_before_lossless_item_details() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("projects.json"), serde_json::to_vec(&json!({
            "local-projects": {"workspace": {"id":"workspace", "name":"Workspace", "rootPaths":[directory.path()]}}
        })).unwrap()).unwrap();
        let (mut relay_server, endpoint) = phoenix::start().await;
        let identity = HostIdentity::from_pkcs8(&key()).unwrap();
        let authentication = Arc::new(Mutex::new(DeviceAuthenticationState::load(directory.path().join("devices.json")).unwrap()));
        let server = Arc::new(CodexAppServer::spawn(AppServerConfig { program: fixture_program(directory.path()), ..AppServerConfig::default() }).await.unwrap());
        let service = CodexRpcService::new(server.clone(), DesktopProjectStore::new(directory.path().join("projects.json")));
        let gateway = Arc::new(EncryptedGateway::new(&identity, authentication.clone(), service.clone()));
        let (mut connections, runner) = relay_transport::connect_runner(&endpoint).await.unwrap();
        let serving = tokio::spawn(async move {
            let mut sessions = JoinSet::new();
            while let Some(connection) = connections.recv().await {
                let gateway = gateway.clone();
                sessions.spawn(async move { let _ = gateway.serve(connection).await; });
            }
            while sessions.join_next().await.is_some() {}
        });
        let ticket = authentication.lock().await.invite(identity.public_key(), "Mac".into(), endpoint.clone(), now()).unwrap().ticket;
        let mobile = MobileClient::connect(config(&endpoint, identity.public_key(), Some(ticket)), &key()).await.unwrap();
        let started = mobile.request("host/thread/start", json!({"cwd":directory.path().join("large-history")})).await.unwrap();
        assert_eq!(started["thread"]["projectId"], "workspace");
        let thread = &started["thread"]["id"];
        let listed = mobile.request("host/thread/list", json!({"limit":20})).await.unwrap();
        assert_eq!(listed["data"][0]["id"], *thread);
        assert_eq!(listed["data"][0]["projectId"], "workspace");
        let start = std::time::Instant::now();
        let preview = mobile.request("host/thread/read", json!({"threadId":thread,"includeTurns":true,"deferItemDetails":true})).await.unwrap();
        let preview_bytes = serde_json::to_vec(&preview).unwrap().len();
        assert_eq!(preview["thread"]["projectId"], "workspace");
        println!("large history preview: {preview_bytes} bytes, {} ms", start.elapsed().as_millis());
        // A byte budget is deterministic; machine speed and network scheduling are not.
        assert!(preview_bytes < 16 * 1024, "collapsed output must not delay the conversation: {preview_bytes} bytes");
        let turn = &preview["thread"]["turns"][0];
        assert_eq!(turn["items"][0]["content"][0]["text"], "Read the whole output");
        assert_eq!(turn["items"][2]["text"], "Large history is complete");
        assert_eq!(turn["deferredItemIds"], json!(["large-command"]));
        let full = mobile.request("host/thread/read", json!({"threadId":thread,"includeTurns":true})).await.unwrap();
        assert!(full["thread"]["turns"][0].get("deferredItemIds").is_none(), "full-history clients must retain inline details");
        let detail = mobile.request("host/thread/item/read", json!({"threadId":thread,"turnId":"large-turn","itemId":"large-command"})).await.unwrap();
        assert_eq!(detail["item"]["aggregatedOutput"], format!("{}END_OF_LARGE_OUTPUT", "output line\n".repeat(700000)));
        assert_eq!(detail["item"], full["thread"]["turns"][0]["items"][1]);
        assert!(mobile.request("host/thread/item/read", json!({"threadId":thread,"turnId":"wrong-turn","itemId":"large-command"})).await.is_err());
        std::fs::write(directory.path().join("list-fixture.json"), serde_json::to_vec(&json!([
            {"id":"fixture-long-history","cwd":directory.path(),"historyMode":"paginated","updatedAt":1}
        ])).unwrap()).unwrap();
        mobile.request("host/thread/list", json!({"useStateDbOnly":true})).await.unwrap();
        let mut page = mobile.request("host/thread/read", json!({"threadId":"fixture-long-history","includeTurns":true,"paginateHistory":true})).await.unwrap();
        assert_eq!(page["thread"]["turns"].as_array().unwrap().len(), 5);
        assert_eq!(page["thread"]["turns"].as_array().unwrap().iter().map(|t| t["items"].as_array().unwrap().len()).sum::<usize>(), 500);
        assert_eq!(page["thread"]["turns"][4]["items"][153]["id"], "long-latest-message");
        let mut turn_ids = std::collections::HashSet::new();
        let mut item_ids = std::collections::HashSet::new();
        loop {
            for turn in page["thread"]["turns"].as_array().unwrap() {
                assert!(turn_ids.insert(turn["id"].as_str().unwrap().to_owned()));
                let mut items = turn.clone();
                loop {
                    assert!(items["items"].as_array().unwrap().len() <= 500);
                    for item in items["items"].as_array().unwrap() {
                        assert!(item_ids.insert(item["id"].as_str().unwrap().to_owned()), "repeated history item");
                    }
                    if items["itemsHasMore"] != true { break; }
                    let cursor = &items["itemsNextCursor"];
                    items = mobile.request("host/thread/items/list", json!({"threadId":"fixture-long-history","turnId":turn["id"],"cursor":cursor})).await.unwrap()["thread"]["turns"][0].take();
                }
            }
            let Some(cursor) = page["thread"]["historyCursor"].as_str() else { break; };
            page = mobile.request("host/thread/turns/list", json!({"threadId":"fixture-long-history","cursor":cursor})).await.unwrap();
        }
        assert_eq!(turn_ids.len(), 10);
        assert_eq!(item_ids.len(), 3718);
        mobile.close(); drop(runner); serving.await.unwrap(); drop(service);
        Arc::try_unwrap(server).ok().expect("Codex process retained").shutdown().await.unwrap();
        relay_server.kill().await.unwrap(); relay_server.wait().await.unwrap();
    }).await.expect("large history loop exceeded deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn title_lists_are_recent_scoped_small_and_expand_without_loading_bodies() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let directory = tempfile::tempdir().unwrap();
        let mut projects = serde_json::Map::new();
        let mut threads = Vec::new();
        for project in 1..=7 {
            let id = format!("project-{project}");
            let cwd = directory.path().join(&id);
            projects.insert(id.clone(), json!({"id":id,"name":format!("Project {project:02}"),"rootPaths":[cwd]}));
            for index in 1..=18 {
                threads.push(json!({"id":format!("p{project}-{index}"),"cwd":cwd,"name":format!("Project {project:02} conversation {index:02}"),"updatedAt":project*100+index,"preview":"unused history".repeat(1000)}));
            }
        }
        for index in 1..=18 {
            threads.push(json!({"id":format!("chat-{index}"),"cwd":directory.path().join("unassigned"),"name":format!("Chat {index:02}"),"updatedAt":index}));
        }
        threads.extend([
            json!({"id":"explicit","cwd":directory.path().join("unassigned"),"name":"Explicit assignment","updatedAt":90000}),
            json!({"id":"projectless","cwd":directory.path().join("project-7"),"name":"Explicit chat","updatedAt":90001}),
            json!({"id":"worktree","cwd":directory.path().join("worktree"),"name":"Worktree conversation","updatedAt":90002}),
        ]);
        let rollout = directory.path().join("external-rollout.jsonl");
        std::fs::write(&rollout, "initial\n").unwrap();
        let external = threads.iter_mut().find(|thread| thread["id"] == "p5-1").unwrap();
        external["historyMode"] = json!("paginated");
        external["status"] = json!({"type":"notLoaded"});
        external["path"] = json!(rollout);
        std::fs::write(directory.path().join("list-fixture.json"), serde_json::to_vec(&threads).unwrap()).unwrap();
        std::fs::write(directory.path().join("projects.json"), serde_json::to_vec(&json!({
            "local-projects":projects,
            "thread-project-assignments":{"explicit":{"projectId":"project-3"}},
            "projectless-thread-ids":["projectless"],
            "thread-workspace-root-hints":{"worktree":directory.path().join("project-5")}
        })).unwrap()).unwrap();
        let (mut relay_server, endpoint) = phoenix::start().await;
        let identity = HostIdentity::from_pkcs8(&key()).unwrap();
        let authentication = Arc::new(Mutex::new(DeviceAuthenticationState::load(directory.path().join("devices.json")).unwrap()));
        let server = Arc::new(CodexAppServer::spawn(AppServerConfig { program: fixture_program(directory.path()), ..AppServerConfig::default() }).await.unwrap());
        let service = CodexRpcService::new(server.clone(), DesktopProjectStore::new(directory.path().join("projects.json")));
        let gateway = Arc::new(EncryptedGateway::new(&identity, authentication.clone(), service.clone()));
        let (mut connections, runner) = relay_transport::connect_runner(&endpoint).await.unwrap();
        let serving = tokio::spawn(async move {
            let mut sessions = JoinSet::new();
            while let Some(connection) = connections.recv().await {
                let gateway = gateway.clone();
                sessions.spawn(async move { let _ = gateway.serve(connection).await; });
            }
            while sessions.join_next().await.is_some() {}
        });
        let ticket = authentication.lock().await.invite(identity.public_key(), "Mac".into(), endpoint.clone(), now()).unwrap().ticket;
        let mobile = MobileClient::connect(config(&endpoint, identity.public_key(), Some(ticket)), &key()).await.unwrap();
        let request = |project_limit, chat_limit, thread_limit| json!({"titleOnly":true,"projectLimit":project_limit,"chatLimit":chat_limit,"projectThreadLimits":{"project-5":thread_limit}});
        let start = std::time::Instant::now();
        let first = mobile.request("host/thread/list", request(5, 5, 5)).await.unwrap();
        let bytes = serde_json::to_vec(&first).unwrap().len();
        println!("title list through encrypted relay: {bytes} bytes, {} ms", start.elapsed().as_millis());
        assert!(bytes < 16 * 1024, "initial titles exceeded the transfer budget");
        let rows = first["data"].as_array().unwrap();
        assert_eq!(rows.len(), 30);
        assert_eq!(first["projects"].as_array().unwrap().len(), 5);
        assert_eq!(first["hasMoreProjects"], true);
        assert_eq!(first["projects"].as_array().unwrap().iter().take(5).map(|project| project["id"].as_str().unwrap()).collect::<Vec<_>>(), ["project-5", "project-3", "project-7", "project-6", "project-4"]);
        assert_eq!(rows[0]["id"], "worktree");
        assert_eq!(rows[4]["id"], "p5-15");
        assert_eq!(rows[5]["id"], "explicit");
        assert_eq!(rows[25]["id"], "projectless");
        assert_eq!(rows[29]["id"], "chat-15");
        assert!(rows.iter().all(|thread| thread.get("turns").is_none() && thread.get("preview").is_none()));
        assert_eq!(first["moreProjectIds"].as_array().unwrap().len(), 5);
        assert_eq!(first["hasMoreChats"], true);

        let more = mobile.request("host/thread/list", request(5, 5, 15)).await.unwrap();
        assert_eq!(more["data"].as_array().unwrap().len(), 40);
        assert_eq!(more["data"][14]["id"], "p5-5");
        let end = mobile.request("host/thread/list", request(15, 25, 25)).await.unwrap();
        assert_eq!(end["data"].as_array().unwrap().len(), 19 + 6*5 + 19);
        assert_eq!(end["hasMoreChats"], false);
        assert_eq!(end["hasMoreProjects"], false);
        assert!(!end["moreProjectIds"].as_array().unwrap().contains(&json!("project-5")));
        let found = mobile.request("host/thread/list", json!({"titleOnly":true,"searchTerm":"Project 01"})).await.unwrap();
        assert_eq!(found["projects"].as_array().unwrap().len(), 1);
        assert_eq!(found["data"].as_array().unwrap().len(), 5);
        assert_eq!(found["data"][0]["id"], "p1-18");
        let body = mobile.request("host/thread/read", json!({"threadId":"p5-1","includeTurns":true})).await.unwrap();
        assert_eq!(body["thread"]["turns"][0]["items"][0]["text"], "History for Project 05 conversation 01");
        assert_eq!(body["thread"]["status"]["type"], "notLoaded");
        let item = mobile.request("host/thread/item/read", json!({"threadId":"p5-1","turnId":"turn-p5-1","itemId":"answer-p5-1"})).await.unwrap();
        assert_eq!(item["item"]["text"], "History for Project 05 conversation 01");
        let mut changes = mobile.subscribe();
        mobile.request("host/thread/watch", json!({"watchId":1,"threadId":"p5-1","path":rollout})).await.unwrap();
        std::fs::write(&rollout, "external client persisted a reply\n").unwrap();
        let changed = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let event: Value = serde_json::from_str(&changes.recv().await.unwrap()).unwrap();
                if event["method"] == "host/thread/changed" { break event; }
            }
        }).await.expect("rollout changes must cross the encrypted relay");
        assert_eq!(changed["params"], json!({"watchId":1,"threadId":"p5-1"}));
        mobile.request("host/thread/unwatch", json!({"watchId":1})).await.unwrap();
        mobile.close(); drop(runner); serving.await.unwrap(); drop(service);
        Arc::try_unwrap(server).ok().expect("Codex process retained").shutdown().await.unwrap();
        relay_server.kill().await.unwrap(); relay_server.wait().await.unwrap();
    }).await.expect("title list loop exceeded deadline");
}
