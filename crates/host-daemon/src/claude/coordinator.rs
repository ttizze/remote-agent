//! Claude project conversations use the channel API, not a local CLI transcript.
//! Authentication remains in the selected Claude Code account's native storage.
use super::process::Process;
use crate::host_rpc::{
    agent::{AgentChange, AgentEvent, emit},
    service::Failure,
};
use agent_protocol::{
    execution::*,
    items::*,
    models::{Project, ProjectMembership, Thread, ThreadResponse, Turn},
    operations as op,
    session::{Capabilities, HistoryPage, ProviderKind, SessionChange, SessionRef},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

pub(crate) fn is_channel(id: &str) -> bool {
    id.strip_prefix("chan_").is_some_and(|suffix| {
        !suffix.is_empty()
            && suffix.len() <= 120
            && suffix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

#[derive(Clone)]
pub(super) struct Client {
    home: PathBuf,
    native_home: PathBuf,
    native_inherited: bool,
    program: PathBuf,
    http: reqwest::Client,
    refresh: Arc<Mutex<()>>,
    base: String,
}

impl Client {
    pub fn new(
        home: PathBuf,
        native_home: PathBuf,
        native_inherited: bool,
        program: PathBuf,
    ) -> Result<Self, Failure> {
        // reqwest's rustls-no-provider feature requires an explicit provider.
        // Project listing can reach this before dictation or APNs initializes TLS.
        let _ = rustls::crypto::ring::default_provider().install_default();
        Ok(Self {
            home,
            native_home,
            native_inherited,
            program,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(30))
                .build()
                .map_err(|_| {
                    Failure::new("coordinator_network", "Claudeへの接続を準備できません。")
                })?,
            refresh: Arc::new(Mutex::new(())),
            base: "https://api.anthropic.com".into(),
        })
    }

    async fn credentials(&self) -> Result<Value, Failure> {
        let path = self.home.join(".credentials.json");
        let metadata = tokio::fs::metadata(&path).await.map_err(|_| auth_error())?;
        if metadata.len() > 65536 {
            return Err(auth_error());
        }
        let bytes = tokio::fs::read(path).await.map_err(|_| auth_error())?;
        let mut value: Value = serde_json::from_slice(&bytes).map_err(|_| auth_error())?;
        let oauth = value.get_mut("claudeAiOauth").ok_or_else(auth_error)?;
        Ok(oauth.take())
    }

    async fn token(&self) -> Result<String, Failure> {
        let _refresh = self.refresh.lock().await;
        let mut credentials = self.credentials().await?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        if credentials["expiresAt"]
            .as_u64()
            .is_none_or(|expiry| expiry <= now + 60_000)
        {
            // Let the official SDK refresh and persist its own credentials.
            let cwd = tempfile::tempdir().map_err(|_| auth_error())?;
            let (process, _) = Process::start(
                &self.program,
                super::process::homes(&self.native_home, self.native_inherited, &self.home),
                cwd.path(),
                None,
                None,
                None,
            )
            .await
            .map_err(|_| auth_error())?;
            process.finish().await.map_err(|_| auth_error())?;
            credentials = self.credentials().await?;
        }
        credentials["accessToken"]
            .as_str()
            .filter(|token| !token.is_empty())
            .map(str::to_owned)
            .ok_or_else(auth_error)
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<Value>,
    ) -> Result<Value, Failure> {
        let write = method != reqwest::Method::GET;
        let token = self.token().await?;
        let mut url = reqwest::Url::parse(&format!("{}{}", self.base, path))
            .map_err(|_| invalid_response())?;
        url.query_pairs_mut().extend_pairs(query.iter().copied());
        let mut request = self
            .http
            .request(method, url)
            .bearer_auth(token)
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", "oauth-2025-04-20");
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|_| {
            if write {
                Failure::unknown(
                    "coordinator_delivery_unknown",
                    "Claudeへの送信結果を確認できません。履歴を更新して確認してください。",
                )
            } else {
                Failure::new(
                    "coordinator_network",
                    "メイン会話を取得できません。接続を確認して再試行してください。",
                )
            }
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(if status.as_u16() == 401 || status.as_u16() == 403 {
                auth_error()
            } else if write && status.is_server_error() {
                Failure::unknown(
                    "coordinator_delivery_unknown",
                    "Claudeへの送信結果を確認できません。履歴を更新して確認してください。",
                )
            } else {
                Failure::new(
                    "coordinator_request_failed",
                    format!(
                        "メイン会話の操作に失敗しました（HTTP {}）。",
                        status.as_u16()
                    ),
                )
            });
        }
        let mut response = response;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| {
            Failure::unknown(
                "coordinator_response_unavailable",
                "Claudeの応答を読み取れません。",
            )
        })? {
            if bytes.len().saturating_add(chunk.len()) > 16 * 1024 * 1024 {
                return Err(Failure::new(
                    "coordinator_response_too_large",
                    "メイン会話の応答が大きすぎます。",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| {
            if write {
                Failure::unknown(
                    "coordinator_response_invalid",
                    "Claudeの送信応答を確認できません。履歴を更新してください。",
                )
            } else {
                Failure::new(
                    "coordinator_response_invalid",
                    "メイン会話の応答を読み取れません。",
                )
            }
        })
    }

    pub async fn projects(&self) -> Result<Vec<Project>, Failure> {
        let mut projects = Vec::new();
        let mut cursor = String::new();
        let mut seen = std::collections::HashSet::new();
        loop {
            let mut query = vec![("scope", "joined"), ("limit", "200")];
            if !cursor.is_empty() {
                query.push(("cursor", &cursor));
            }
            let page = self
                .request(reqwest::Method::GET, "/v1/code/channels", &query, None)
                .await?;
            for row in page["data"].as_array().ok_or_else(invalid_response)? {
                let Some(id) = row["id"].as_str().filter(|id| is_channel(id)) else {
                    continue;
                };
                let Some(name) = row["name"].as_str().filter(|name| !name.trim().is_empty()) else {
                    continue;
                };
                if row["archived_at"].is_string() || row["disabled_at"].is_string() {
                    continue;
                }
                projects.push(Project {
                    id: format!("claude-project:{id}"),
                    name: name.into(),
                    ..Default::default()
                });
            }
            let Some(next) = page["next_cursor"]
                .as_str()
                .filter(|cursor| !cursor.is_empty())
            else {
                break;
            };
            if !seen.insert(next.to_owned()) || projects.len() > 20_000 {
                return Err(invalid_response());
            }
            cursor = next.into();
        }
        Ok(projects)
    }

    pub async fn page(
        &self,
        id: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<HistoryPage, Failure> {
        if !is_channel(id) {
            return Err(invalid_response());
        }
        let limit = limit.clamp(1, 200).to_string();
        let mut query = vec![("limit", limit.as_str()), ("include_draft", "true")];
        if let Some(cursor) = cursor {
            query.push(("cursor", cursor));
        }
        let value = self
            .request(
                reqwest::Method::GET,
                &format!("/v1/code/channels/{id}/timeline"),
                &query,
                None,
            )
            .await?;
        timeline(&value, id)
    }

    pub async fn open(&self, id: &str, limit: usize) -> Result<ThreadResponse, Failure> {
        if !is_channel(id) {
            return Err(invalid_response());
        }
        let channel = self
            .request(
                reqwest::Method::GET,
                &format!("/v1/code/channels/{id}"),
                &[],
                None,
            )
            .await?;
        let channel = &channel["channel"];
        if channel["id"] != id {
            return Err(invalid_response());
        }
        let page = self.page(id, None, limit).await?;
        let status = if page
            .turns
            .iter()
            .any(|turn| turn.status == TurnStatus::Running)
        {
            agent_protocol::models::SessionStatus::Running
        } else {
            agent_protocol::models::SessionStatus::Idle
        };
        Ok(ThreadResponse {
            thread: Thread {
                id: Some(SessionRef {
                    provider: ProviderKind::Claude,
                    id: id.into(),
                }),
                name: Some(format!(
                    "メイン会話 · {}",
                    channel["name"].as_str().unwrap_or("Claude")
                )),
                project_id: ProjectMembership::Assigned(format!("claude-project:{id}")),
                can_accept_direct_input: Some(true),
                capabilities: Some(Capabilities {
                    additional_input: true,
                    fork: false,
                    rename: false,
                    model_change: false,
                }),
                status,
                history_has_more: Some(page.next_cursor.is_some()),
                history_cursor: page.next_cursor,
                history_limit: Some(limit as u64),
                turns: Some(page.turns),
                ..Default::default()
            },
            model: None,
        })
    }

    pub async fn send(&self, input: &op::Submission) -> Result<op::SubmissionReceipt, Failure> {
        let id = &input.thread_id.id;
        if !is_channel(id) {
            return Err(invalid_response());
        }
        let text = text_input(&input.input)?;
        self.request(
            reqwest::Method::POST,
            &format!("/v1/code/channels/{id}/messages"),
            &[],
            Some(
                json!({ "body": text, "client_message_id": input.client_user_message_id.as_str() }),
            ),
        )
        .await?;
        // The channel returns durable message acceptance; it owns execution separately.
        Ok(op::SubmissionReceipt { turn_id: None })
    }
}

fn auth_error() -> Failure {
    Failure::new(
        "coordinator_auth",
        "BEXのClaudeアカウントでメイン会話へ接続できません。アカウント設定からClaudeへログインし直してください。",
    )
}
fn invalid_response() -> Failure {
    Failure::new(
        "coordinator_response_invalid",
        "メイン会話の応答形式を確認できません。",
    )
}

fn text_input(input: &[op::Input]) -> Result<String, Failure> {
    let mut text = Vec::new();
    for part in input {
        match part {
            op::Input::Text { text: value } => text.push(value.as_str()),
            _ => {
                return Err(Failure::new(
                    "coordinator_input_unavailable",
                    "メイン会話ではテキストを送信してください。添付ファイルはまだ対応していません。",
                ));
            }
        }
    }
    let text = text.join("\n");
    if text.trim().is_empty() {
        return Err(Failure::new(
            "invalid_input",
            "メッセージを入力してください。",
        ));
    }
    Ok(text)
}

pub(super) fn timeline(value: &Value, channel: &str) -> Result<HistoryPage, Failure> {
    let mut turns = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut rows: Vec<_> = value["data"]
        .as_array()
        .ok_or_else(invalid_response)?
        .iter()
        .collect();
    // Native pages are newest first. Preserve the server's order at equal timestamps.
    rows.reverse();
    rows.sort_by_key(|row| {
        row["created_at"]
            .as_str()
            .and_then(|date| chrono::DateTime::parse_from_rfc3339(date).ok())
    });
    for row in rows {
        if row["channel_id"] != channel {
            return Err(invalid_response());
        }
        if row["private_to_account_id"].is_string() {
            continue;
        }
        if let Some(turn) = message(row)?
            && seen.insert(turn.id.clone())
        {
            turns.push(Arc::new(turn));
        }
    }
    Ok(HistoryPage {
        turns,
        next_cursor: value["next_cursor"]
            .as_str()
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
    })
}

fn message(row: &Value) -> Result<Option<Turn>, Failure> {
    let id = row["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(invalid_response)?;
    let mut text = row["body"]
        .as_str()
        .ok_or_else(invalid_response)?
        .to_owned();
    for attachment in row["attachments"].as_array().into_iter().flatten() {
        if let Some(name) = attachment["filename"].as_str() {
            text.push_str(&format!("\n\n📎 {name}"));
        }
    }
    if text.trim().is_empty() {
        return Ok(None);
    }
    let body = if row["author_account_id"].is_string() {
        ItemBody::UserMessage {
            text: Some(text),
            content: Vec::new(),
        }
    } else {
        ItemBody::AssistantText {
            text,
            phase: if row["server_notice"] == true {
                AssistantPhase::Commentary
            } else {
                AssistantPhase::Final
            },
        }
    };
    let mut item = Item::new(id.into(), ItemStatus::Completed, body);
    if row["author_account_id"].is_string() {
        item.client_input_id = row["client_message_id"].as_str().map(Into::into);
    }
    let timestamp = row["created_at"]
        .as_str()
        .and_then(|date| chrono::DateTime::parse_from_rfc3339(date).ok())
        .map(|date| date.timestamp_millis().max(0) as u64);
    Ok(Some(Turn {
        id: id.into(),
        status: TurnStatus::Completed,
        items: Some(vec![Arc::new(item)]),
        started_at_ms: timestamp,
        ..Default::default()
    }))
}

#[derive(Default)]
pub(super) struct Coordinators {
    watching: Arc<Mutex<HashMap<String, (uuid::Uuid, CancellationToken)>>>,
    tasks: Mutex<tokio::task::JoinSet<()>>,
    projects: Mutex<Option<(PathBuf, Instant, Vec<Project>)>>,
    client: Mutex<Option<Client>>,
}

impl Coordinators {
    pub async fn client(
        &self,
        home: PathBuf,
        native_home: PathBuf,
        native_inherited: bool,
        program: PathBuf,
    ) -> Result<Client, Failure> {
        let mut cached = self.client.lock().await;
        if let Some(client) = &*cached
            && client.home == home
        {
            return Ok(client.clone());
        }
        let client = Client::new(home, native_home, native_inherited, program)?;
        *cached = Some(client.clone());
        Ok(client)
    }
    pub async fn projects(&self, client: &Client) -> Result<Vec<Project>, Failure> {
        let mut cache = self.projects.lock().await;
        if let Some((home, at, projects)) = &*cache
            && home == &client.home
            && at.elapsed() < Duration::from_secs(60)
        {
            return Ok(projects.clone());
        }
        match client.projects().await {
            Ok(projects) => {
                *cache = Some((client.home.clone(), Instant::now(), projects.clone()));
                Ok(projects)
            }
            Err(error) => match &*cache {
                Some((home, _, projects)) if home == &client.home => Ok(projects.clone()),
                _ => Err(error),
            },
        }
    }

    pub async fn cancel(&self) {
        for (_, (_, stop)) in self.watching.lock().await.drain() {
            stop.cancel();
        }
        *self.projects.lock().await = None;
        *self.client.lock().await = None;
    }

    pub async fn watch(
        &self,
        client: Client,
        response: &ThreadResponse,
        events: mpsc::Sender<AgentEvent>,
        stop: CancellationToken,
    ) {
        let id = response.thread.id.as_ref().unwrap().id.clone();
        let mut watching = self.watching.lock().await;
        if watching.contains_key(&id) {
            return;
        }
        if watching.len() >= 16
            && let Some(key) = watching.keys().next().cloned()
            && let Some((_, old)) = watching.remove(&key)
        {
            old.cancel();
        }
        let instance = uuid::Uuid::new_v4();
        let cancel = stop.child_token();
        watching.insert(id.clone(), (instance, cancel.clone()));
        let watchers = self.watching.clone();
        let mut previous: BTreeMap<_, _> = response
            .thread
            .turns
            .iter()
            .flatten()
            .map(|turn| (turn.id.clone(), turn.clone()))
            .collect();
        self.tasks.lock().await.spawn(async move {
            loop {
                tokio::select! { _ = cancel.cancelled() => break, _ = tokio::time::sleep(Duration::from_secs(3)) => {} }
                let session = SessionRef { provider: ProviderKind::Claude, id: id.clone() };
                let observed = tokio::select! { _ = cancel.cancelled() => break, result = emit(&events, AgentChange::Observed { session: session.clone(), changes: Vec::new() }) => result };
                if observed.is_err() { break; }
                let page = tokio::select! { _ = cancel.cancelled() => break, result = client.page(&id, None, 100) => result };
                let Ok(page) = page else { continue };
                let mut changes = Vec::new();
                let mut current = BTreeMap::new();
                for turn in page.turns {
                    if previous.get(&turn.id) != Some(&turn) {
                        changes.push(SessionChange::Turn { turn: (*turn).clone(), completed: true });
                    }
                    current.insert(turn.id.clone(), turn);
                }
                previous = current;
                if !changes.is_empty() {
                    let observed = tokio::select! { _ = cancel.cancelled() => break, result = emit(&events, AgentChange::Observed { session, changes }) => result };
                    if observed.is_err() { break; }
                }
            }
            let mut watching = watchers.lock().await;
            if watching.get(&id).is_some_and(|(value, _)| *value == instance) { watching.remove(&id); }
        });
    }

    pub async fn shutdown(&self) {
        self.cancel().await;
        let mut tasks = self.tasks.lock().await;
        while tasks.join_next().await.is_some() {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn cold_coordinator_initializes_tls_without_another_feature_running_first() {
        const CHILD: &str = "BEX_TEST_COLD_COORDINATOR_TLS";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "claude::coordinator::tests::cold_coordinator_initializes_tls_without_another_feature_running_first",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        assert!(rustls::crypto::CryptoProvider::get_default().is_none());
        let directory = tempfile::tempdir().unwrap();
        Client::new(
            directory.path().into(),
            directory.path().into(),
            false,
            "claude".into(),
        )
        .unwrap();
        // Repeated initialization must also be safe.
        Client::new(
            directory.path().into(),
            directory.path().into(),
            false,
            "claude".into(),
        )
        .unwrap();
    }

    fn row(id: &str, time: &str, human: bool, text: &str) -> Value {
        let mut row = json!({"id":id,"channel_id":"chan_fixture","body":text,"created_at":time,"seq":"0","attachments":[]});
        if human {
            row["author_account_id"] = json!("user_fixture");
            row["client_message_id"] = json!("local-input");
        }
        row
    }

    #[test]
    fn real_channel_shape_is_mapped_to_regular_bex_messages_and_native_cursor() {
        let page = timeline(&json!({"data":[
            row("answer", "2026-10-21T10:20:12.078351Z", false, "## 方針\n\n**履歴**を表示します。"),
            row("question", "2026-10-21T10:20:05.539581Z", true, "現在の進捗は？")
        ],"next_cursor":"opaque+cursor/one"}), "chan_fixture").unwrap();
        assert_eq!(page.next_cursor.as_deref(), Some("opaque+cursor/one"));
        assert_eq!(
            page.turns
                .iter()
                .map(|turn| turn.id.as_str())
                .collect::<Vec<_>>(),
            ["question", "answer"]
        );
        let question = &page.turns[0].items.as_ref().unwrap()[0];
        assert!(
            matches!(question.body(), ItemBody::UserMessage { text: Some(text), .. } if text == "現在の進捗は？")
        );
        assert_eq!(
            question.client_input_id.as_ref().unwrap().as_str(),
            "local-input"
        );
        assert!(
            matches!(page.turns[1].items.as_ref().unwrap()[0].body(), ItemBody::AssistantText { text, .. } if text.starts_with("## 方針"))
        );
    }

    #[test]
    fn timestamp_ties_keep_native_order_and_other_private_rows_never_cross_clients() {
        let mut private = row("private", "2026-10-21T10:20:12.000000Z", true, "secret");
        private["private_to_account_id"] = json!("another-account");
        let page = timeline(
            &json!({"data":[private,
                row("a", "2026-10-21T10:20:12.000002Z", false, "Second"),
                row("z", "2026-10-21T10:20:12.000001Z", false, "First")
            ]}),
            "chan_fixture",
        )
        .unwrap();
        assert_eq!(
            page.turns
                .iter()
                .map(|turn| turn.id.as_str())
                .collect::<Vec<_>>(),
            ["z", "a"]
        );
        assert!(
            timeline(
                &json!({"data":[row("x","",false,"wrong project")]}),
                "chan_other"
            )
            .is_err()
        );
        for input in [json!({"error":"not logged in"}), json!({})] {
            assert!(timeline(&input, "chan_fixture").is_err());
        }
    }

    #[test]
    fn coordinator_input_validation_never_silently_discards_attachments() {
        assert_eq!(
            text_input(&[
                op::Input::Text {
                    text: "Hello".into()
                },
                op::Input::Text {
                    text: "Again".into()
                }
            ])
            .unwrap(),
            "Hello\nAgain"
        );
        assert!(text_input(&[]).is_err());
        assert!(text_input(&[op::Input::Text { text: " ".into() }]).is_err());
        assert!(
            text_input(&[op::Input::LocalImage {
                path: "/private.png".into()
            }])
            .is_err()
        );
        for id in ["", "chan_", "chan_one/two", "chan_one?other", "../chan_one"] {
            assert!(!is_channel(id));
        }
    }

    proptest::proptest! {
        #[test]
        fn valid_channel_ids_cannot_escape_the_channel_endpoint(suffix in "[A-Za-z0-9_-]{1,120}") {
            let id = format!("chan_{suffix}");
            proptest::prop_assert!(is_channel(&id));
            let url = reqwest::Url::parse(&format!("https://api.anthropic.com/v1/code/channels/{id}/timeline")).unwrap();
            proptest::prop_assert_eq!(url.path_segments().unwrap().collect::<Vec<_>>(), vec!["v1", "code", "channels", &id, "timeline"]);
        }
    }

    #[tokio::test]
    async fn api_reads_and_sends_use_selected_account_and_stable_client_id_without_a_workspace() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join(".credentials.json"),
            json!({"claudeAiOauth":{"accessToken":"test-only-token","expiresAt":u64::MAX}})
                .to_string(),
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut captured = Vec::new();
            for body in [
                json!({"channel":{"id":"chan_fixture","name":"Project"}}),
                json!({"data":[row("message","2026-10-21T00:00:00Z",false,"Native answer")]}),
                json!({"id":"accepted"}),
            ] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut buffer = [0; 4096];
                    let length = stream.read(&mut buffer).await.unwrap();
                    if length == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..length]);
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&request[..end]);
                        let length = header
                            .lines()
                            .find_map(|line| {
                                line.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|s| s.trim().parse::<usize>().ok())
                            })
                            .unwrap_or_default();
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                captured.push(String::from_utf8(request).unwrap());
                let body = body.to_string();
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).as_bytes()).await.unwrap();
            }
            captured
        });
        let mut client = Client::new(
            directory.path().into(),
            directory.path().into(),
            false,
            "unused-cli".into(),
        )
        .unwrap();
        client.base = format!("http://{address}");
        let opened = client.open("chan_fixture", 100).await.unwrap();
        assert_eq!(opened.thread.name.as_deref(), Some("メイン会話 · Project"));
        assert!(opened.thread.cwd.is_none());
        assert!(!opened.thread.capabilities.unwrap().model_change);
        let receipt = client
            .send(&op::Submission {
                thread_id: SessionRef {
                    provider: ProviderKind::Claude,
                    id: "chan_fixture".into(),
                },
                client_user_message_id: "same-input-id".into(),
                input: vec![op::Input::Text {
                    text: "Continue".into(),
                }],
                model: None,
                effort: None,
                service_tier: None,
            })
            .await
            .unwrap();
        assert!(receipt.turn_id.is_none());
        let captured = server.await.unwrap();
        assert!(
            captured
                .iter()
                .all(|request| request.contains("Bearer test-only-token"))
        );
        assert!(captured[1].starts_with(
            "GET /v1/code/channels/chan_fixture/timeline?limit=100&include_draft=true"
        ));
        let sent: Value =
            serde_json::from_str(captured[2].split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(
            sent,
            json!({"body":"Continue","client_message_id":"same-input-id"})
        );
    }
}
