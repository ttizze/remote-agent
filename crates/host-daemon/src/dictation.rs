use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use crate::{codex_accounts::AuthToken, host_rpc::SessionId};
use async_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest, http::HeaderValue},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use codex_app_server::CodexAppServer;
use futures_io::{AsyncRead, AsyncWrite};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{Value, json};
use zeroize::Zeroizing;

// 100ms of PCM16 at 24kHz. Base64 and sample boundaries both remain aligned.
const AUDIO_CHUNK_BASE64_BYTES: usize = 6_400;
const DICTATION_URL: &str = "wss://chatgpt.com/backend-api/dictation/stream";
const API_TRANSCRIBE_URL: &str = "https://api.openai.com/v1/audio/transcriptions";
const TRANSCRIBE_URL: &str = "https://chatgpt.com/backend-api/transcribe";

type DictationSocket = WebSocketStream<async_tungstenite::tokio::ConnectStream>;

struct PreparedConnection {
    token: Zeroizing<String>,
    socket: DictationSocket,
}

struct Prepared {
    id: String,
    ready: tokio::sync::oneshot::Sender<()>,
    task: tokio_util::task::AbortOnDropHandle<Result<PreparedConnection, String>>,
}
impl Prepared {
    fn new(
        id: String,
        connection: impl std::future::Future<Output = Result<PreparedConnection, String>>
        + Send
        + 'static,
    ) -> Self {
        let (ready, recorded) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let PreparedConnection { token, socket } =
                tokio::time::timeout(Duration::from_secs(10), connection)
                    .await
                    .map_err(|_| "音声接続の準備がタイムアウトしました。")??;
            let socket = wait_for_recording(socket, recorded, Duration::from_secs(300)).await?;
            Ok(PreparedConnection { token, socket })
        });
        Self {
            id,
            ready,
            task: tokio_util::task::AbortOnDropHandle::new(task),
        }
    }
    async fn take(self) -> Option<PreparedConnection> {
        let _ = self.ready.send(());
        self.task.await.ok()?.ok()
    }
}

// One recording per authenticated client. Replacing or cancelling a recording
// drops its task and socket; another client's preparation remains independent.
pub(crate) struct Dictation {
    backend: Result<Arc<CodexAppServer>, String>,
    prepared: Mutex<HashMap<SessionId, Prepared>>,
}
impl Dictation {
    pub(crate) fn new(backend: Result<Arc<CodexAppServer>, String>) -> Self {
        Self {
            backend,
            prepared: Default::default(),
        }
    }
    pub(crate) fn prepare(&self, session: SessionId, id: String) -> Result<(), String> {
        uuid::Uuid::parse_str(&id).map_err(|_| "録音の識別子が無効です。")?;
        let app_server = self
            .backend
            .as_ref()
            .map_err(|_| "音声入力のCodexバックエンドを利用できません。")?
            .clone();
        let connection = async move {
            let AuthToken::ChatGpt(token) =
                crate::codex_accounts::auth_token(&app_server, false).await?
            else {
                // API-key transcription is a single HTTP file request. It has no
                // idle WebSocket to prepare; keep that request at recording stop.
                return Err("APIキーの音声入力は録音後に接続します。".into());
            };
            let socket = connect_stream(
                &token,
                &app_server.initialize_response().user_agent,
                DICTATION_URL,
            )
            .await?;
            Ok(PreparedConnection { token, socket })
        };
        self.prepared
            .lock()
            .unwrap()
            .insert(session, Prepared::new(id, connection));
        Ok(())
    }
    pub(crate) fn cancel(&self, session: SessionId, id: &str) {
        drop(self.take_preparation(session, Some(id)));
    }
    pub(crate) fn close_session(&self, session: SessionId) {
        self.prepared.lock().unwrap().remove(&session);
    }
    fn take_preparation(&self, session: SessionId, id: Option<&str>) -> Option<Prepared> {
        let mut prepared = self.prepared.lock().unwrap();
        if id.is_some_and(|id| prepared.get(&session).is_some_and(|entry| entry.id == id)) {
            prepared.remove(&session)
        } else {
            None
        }
    }
    pub(crate) async fn transcribe(
        &self,
        session: SessionId,
        preparation: Option<&str>,
        audio: &[u8],
    ) -> Result<agent_protocol::operations::Transcription, String> {
        let prepared = self.take_preparation(session, preparation);
        let app_server = self
            .backend
            .as_deref()
            .map_err(|error| format!("音声入力のCodexバックエンドを利用できません: {error}"))?;
        tokio::time::timeout(
            Duration::from_secs(25),
            transcribe_request(app_server, audio, prepared),
        )
        .await
        .map_err(|_| "文字起こしがタイムアウトしました。")?
    }
}

async fn wait_for_recording<S>(
    mut socket: WebSocketStream<S>,
    mut recorded: tokio::sync::oneshot::Receiver<()>,
    lifetime: Duration,
) -> Result<WebSocketStream<S>, String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let expires = tokio::time::sleep(lifetime);
    tokio::pin!(expires);
    loop {
        tokio::select! {
            result = &mut recorded => {
                result.map_err(|_| "録音が取り消されました。")?;
                return Ok(socket);
            }
            _ = &mut expires => return Err("音声接続の準備が期限切れになりました。".into()),
            message = socket.next() => match message {
                Some(Ok(Message::Ping(_))) => socket.flush().await.map_err(|_| "音声接続の準備中に接続が切れました。")?,
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return Err("音声接続の準備中に接続が切れました。".into()),
                _ => {},
            }
        }
    }
}

async fn transcribe_request(
    app_server: &CodexAppServer,
    pcm: &[u8],
    prepared: Option<Prepared>,
) -> Result<agent_protocol::operations::Transcription, String> {
    if pcm.is_empty() || !pcm.len().is_multiple_of(2) {
        return Err("録音データが無効です。".into());
    }
    // Re-read the selected account at stop. Never use the prepared socket after
    // an account switch or refreshed token. Credentials stay exclusively on Host.
    let text = match crate::codex_accounts::auth_token(app_server, false).await? {
        AuthToken::ApiKey(key) => {
            drop(prepared);
            transcribe_recording(&key, RecordingService::OpenAi, pcm, API_TRANSCRIBE_URL).await?
        }
        AuthToken::ChatGpt(token) => {
            transcribe_authenticated(
                &token,
                &app_server.initialize_response().user_agent,
                pcm,
                DICTATION_URL,
                TRANSCRIBE_URL,
                prepared,
            )
            .await?
        }
    };
    Ok(agent_protocol::operations::Transcription { text })
}

// Both TLS backends are linked on desktop; iroh does not install a global one.
// Each independent HTTP/WebSocket entry point must work on a cold Host.
fn install_tls_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
}

async fn transcribe_authenticated(
    token: &str,
    user_agent: &str,
    pcm: &[u8],
    stream_url: &str,
    recording_url: &str,
    prepared: Option<Prepared>,
) -> Result<String, String> {
    // Keep complete PCM for file transcription if the WebSocket attempts fail.
    let stream_result = tokio::time::timeout(Duration::from_secs(18), async {
        let audio = Zeroizing::new(STANDARD.encode(pcm));
        // Preparation shares the stream budget so file fallback still has
        // time to finish within the overall transcription timeout.
        let socket = match prepared {
            Some(prepared) => prepared
                .take()
                .await
                .filter(|connection| connection.token.as_str() == token)
                .map(|connection| connection.socket),
            None => None,
        };
        if let Some(socket) = socket
            && let Ok(text) = transcribe_socket(socket, &audio).await
        {
            return Ok(text);
        }
        // An idle prepared connection may have expired. Retry with all of
        // the same recording before falling back to the file endpoint.
        let socket = connect_stream(token, user_agent, stream_url).await?;
        transcribe_socket(socket, &audio).await
    })
    .await
    .unwrap_or_else(|_| Err("音声ストリームがタイムアウトしました。".into()));
    match stream_result {
        Ok(text) => Ok(text),
        Err(stream_error) => transcribe_recording(
            token,
            RecordingService::ChatGpt(user_agent),
            pcm,
            recording_url,
        )
        .await
        .map_err(|error| format!("{stream_error}\n録音ファイルの文字起こし: {error}")),
    }
}

async fn connect_stream(
    token: &str,
    user_agent: &str,
    url: &str,
) -> Result<DictationSocket, String> {
    install_tls_provider();
    let mut request = url
        .into_client_request()
        .map_err(|_| "音声処理の接続先が無効です。")?;
    let protocols = Zeroizing::new(format!(
        "chatgpt-dictation, openai-bearer.{token}, codex-desktop"
    ));
    let mut header =
        HeaderValue::from_str(&protocols).map_err(|_| "Codexの認証情報が無効です。")?;
    header.set_sensitive(true);
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", header);
    request.headers_mut().insert(
        "User-Agent",
        HeaderValue::from_str(user_agent).map_err(|_| "Codexの接続情報が無効です。")?,
    );
    let (socket, _) = async_tungstenite::tokio::connect_async(request)
        .await
        .map_err(|error| match error {
            async_tungstenite::tungstenite::Error::Http(response)
                if response
                    .headers()
                    .get("cf-mitigated")
                    .is_some_and(|value| value == "challenge") =>
            {
                "Codexの音声サービスがブラウザでの確認を要求しているため、接続できません。".into()
            }
            async_tungstenite::tungstenite::Error::Http(response) => format!(
                "Codexの音声処理に接続できませんでした（HTTP {}）。",
                response.status().as_u16()
            ),
            async_tungstenite::tungstenite::Error::Tls(_) => {
                "Codexの音声処理とのTLS接続に失敗しました。".into()
            }
            async_tungstenite::tungstenite::Error::Io(error) => format!(
                "Codexの音声処理に接続できませんでした（通信エラー: {:?}）。",
                error.kind()
            ),
            _ => "Codexの音声処理とのWebSocket接続に失敗しました。".into(),
        })?;
    Ok(socket)
}

enum RecordingService<'a> {
    ChatGpt(&'a str),
    OpenAi,
}

async fn transcribe_recording(
    token: &str,
    service: RecordingService<'_>,
    pcm: &[u8],
    url: &str,
) -> Result<String, String> {
    install_tls_provider();
    static CLIENT: OnceLock<Result<reqwest::Client, reqwest::Error>> = OnceLock::new();
    let client = CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(25))
                .build()
        })
        .as_ref()
        .map_err(|_| "録音ファイルの接続を初期化できませんでした。")?;

    // The WAV container preserves native PCM and declares its capture format.
    let data_len = u32::try_from(pcm.len()).map_err(|_| "録音データが大きすぎます。")?;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt \x10\0\0\0\x01\0\x01\0");
    wav.extend_from_slice(&24_000_u32.to_le_bytes());
    wav.extend_from_slice(&48_000_u32.to_le_bytes());
    wav.extend_from_slice(b"\x02\0\x10\0data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);
    let file = reqwest::multipart::Part::bytes(wav)
        .file_name("codex.wav")
        .mime_str("audio/wav")
        .map_err(|_| "録音形式が無効です。")?;
    let mut form = reqwest::multipart::Form::new().part("file", file);
    let mut request = client.post(url).bearer_auth(token);
    match service {
        RecordingService::OpenAi => {
            form = form.text("model", "gpt-4o-mini-transcribe");
        }
        RecordingService::ChatGpt(user_agent) => {
            request = request
                .header("originator", "Codex Desktop")
                .header("User-Agent", user_agent);
            // Claims select a header only; the service verifies the token.
            if let Some(claims) = crate::codex_accounts::token_claims(token)
                && let Some(account_id) =
                    claims["https://api.openai.com/auth"]["chatgpt_account_id"].as_str()
            {
                let mut account = HeaderValue::from_str(account_id)
                    .map_err(|_| "Codexのアカウント情報が無効です。")?;
                account.set_sensitive(true);
                request = request.header("ChatGPT-Account-Id", account);
            }
        }
    }
    let response = request
        .multipart(form)
        .send()
        .await
        .map_err(|_| "Codexへの録音ファイル送信に失敗しました。")?;
    if !response.status().is_success() {
        return Err(
            if response
                .headers()
                .get("cf-mitigated")
                .is_some_and(|value| value == "challenge")
            {
                "Codexがブラウザでの確認を要求しています。".into()
            } else {
                format!("CodexがHTTP {}を返しました。", response.status().as_u16())
            },
        );
    }
    let mut result: Value = response
        .json()
        .await
        .map_err(|_| "文字起こしの応答が無効です。")?;
    match result["text"].take() {
        Value::String(text) => Ok(text),
        _ => Err("文字起こしの応答が無効です。".into()),
    }
}

#[derive(Serialize)]
struct AudioAppend<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    audio: &'a str,
}

async fn transcribe_socket<S>(mut socket: WebSocketStream<S>, audio: &str) -> Result<String, String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    socket.send(Message::Text(json!({
        "type": "session.start",
        "config": {
            "input_audio_format": "pcm16", "sample_rate_hz": 24000, "num_channels": 1,
            "max_buffer_size_bytes": 4 * 1024 * 1024, "max_utterance_duration_ms": 30000,
            "session_ttl_ms": 300000, "provider_mode": "streaming_sse",
            "transcript_delivery_mode": "final_only",
            "vad": { "type": "server_vad", "threshold": 0.5, "prefix_padding_ms": 300, "silence_duration_ms": 500 }
        }
    }).to_string().into())).await.map_err(|_| "文字起こしを開始できませんでした。")?;

    let mut started = false;
    let mut transcripts: Vec<(String, u64, String)> = Vec::new();
    while let Some(message) = socket.next().await {
        match message.map_err(|_| "音声処理との接続が切れました。")? {
            Message::Text(message) => {
                let event: Value = serde_json::from_str(&message).map_err(|_| "音声処理の応答が無効です。")?;
                match event["type"].as_str() {
                    Some("session.started") if !started => {
                        started = true;
                        // Keep the provider's sample-aligned frames, but batch
                        // writes: this recording is already complete, so flushing
                        // each 100ms frame only adds upload overhead. Sink
                        // backpressure still bounds the WebSocket write buffer.
                        for start in (0..audio.len()).step_by(AUDIO_CHUNK_BASE64_BYTES) {
                            let audio = &audio[start..(start + AUDIO_CHUNK_BASE64_BYTES).min(audio.len())];
                            socket.feed(Message::Text(serde_json::to_string(&AudioAppend { kind: "audio.append", audio })
                                .map_err(|_| "録音データを送信できませんでした。")?.into())).await
                                .map_err(|_| "録音データを送信できませんでした。")?;
                        }
                        socket.send(Message::Text(r#"{"type":"session.close"}"#.into())).await
                            .map_err(|_| "録音データを送信できませんでした。")?;
                    }
                    Some("transcript.final") => {
                        let id = event["utterance_id"].as_str().ok_or("文字起こしの応答が無効です。")?;
                        let revision = event["revision"].as_u64().ok_or("文字起こしの応答が無効です。")?;
                        let text = event["text"].as_str().ok_or("文字起こしの応答が無効です。")?;
                        match transcripts.iter_mut().find(|(key, _, _)| key == id) {
                            Some((_, previous, value)) if revision >= *previous => {
                                *previous = revision;
                                value.clear();
                                value.push_str(text);
                            }
                            Some(_) => {}
                            None => transcripts.push((id.to_owned(), revision, text.to_owned())),
                        }
                    }
                    Some("session.updated") if started && event["session"]["status"] == "closed" => {
                        // The desktop resolves finish on this event, without
                        // waiting for the server to close the WebSocket.
                        let _ = socket.close(None).await;
                        return Ok(transcript_text(transcripts));
                    }
                    Some("transcript.failed") => return Err("Codexで文字起こしできませんでした。".into()),
                    Some("session.error") if event["fatal"] == true => return Err("Codexで文字起こしできませんでした。".into()),
                    _ => {}
                }
            }
            Message::Ping(_) => socket.flush().await.map_err(|_| "音声処理との接続が切れました。")?,
            Message::Close(frame) if started && frame.as_ref().is_some_and(|frame| frame.code == async_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Normal) => {
                return Ok(transcript_text(transcripts));
            }
            Message::Close(_) => return Err("文字起こしが完了する前に接続が切れました。".into()),
            _ => {}
        }
    }
    Err("文字起こしが完了する前に接続が切れました。".into())
}

fn transcript_text(transcripts: Vec<(String, u64, String)>) -> String {
    let mut text = String::new();
    for (_, _, segment) in transcripts {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(segment);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_tungstenite::tungstenite::protocol::{CloseFrame, Role, frame::coding::CloseCode};
    use std::{
        cell::Cell,
        pin::Pin,
        task::{Context, Poll},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[allow(
        clippy::result_large_err,
        reason = "Tungstenite fixes the callback's unboxed ErrorResponse type"
    )]
    async fn accept_provider(
        stream: tokio::net::TcpStream,
    ) -> WebSocketStream<async_tungstenite::tokio::TokioAdapter<tokio::net::TcpStream>> {
        use async_tungstenite::tungstenite::handshake::server::{Request, Response};
        async_tungstenite::tokio::accept_hdr_async(
            stream,
            |request: &Request, mut response: Response| {
                assert!(
                    request.headers()["Sec-WebSocket-Protocol"]
                        .to_str()
                        .unwrap()
                        .starts_with("chatgpt-dictation, openai-bearer.")
                );
                response.headers_mut().insert(
                    "Sec-WebSocket-Protocol",
                    HeaderValue::from_static("chatgpt-dictation"),
                );
                Ok(response)
            },
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn prepares_only_the_connection_then_reuses_it_for_the_complete_recording() {
        tokio::time::timeout(Duration::from_secs(3), async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}/stream", listener.local_addr().unwrap());
            let (alive, warmed) = tokio::sync::oneshot::channel();
            let provider = async {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = accept_provider(stream).await;
                socket.send(Message::Ping(vec![7].into())).await.unwrap();
                assert_eq!(socket.next().await.unwrap().unwrap(), Message::Pong(vec![7].into()),
                    "preparation must service keepalive without starting recognition or sending audio");
                alive.send(()).unwrap();
                provider_recording(socket, "AQD/fw==", vec![
                    json!({"type":"transcript.final","utterance_id":"full","revision":1,"text":"全文"}),
                    json!({"type":"session.updated","session":{"status":"closed"}}),
                ], CloseCode::Normal).await;
                assert!(tokio::time::timeout(Duration::from_millis(20), listener.accept()).await.is_err(),
                    "prepared socket must be reused");
            };
            let operation = async {
                let connection_url = url.clone();
                let prepared = Prepared::new("recording".into(), async move {
                    let token = Zeroizing::new("isolated-token".to_owned());
                    let socket = connect_stream(&token, "isolated-codex/1.0", &connection_url).await?;
                    Ok(PreparedConnection { token, socket })
                });
                warmed.await.unwrap();
                transcribe_authenticated("isolated-token", "isolated-codex/1.0",
                    &[1, 0, 255, 127], &url, "http://127.0.0.1:1/unused", Some(prepared)).await.unwrap()
            };
            let (text, ()) = tokio::join!(operation, provider);
            assert_eq!(text, "全文");
        }).await.expect("prepared transcription stalled");
    }

    #[tokio::test]
    async fn retries_a_lost_prepared_socket_with_all_audio_and_discards_partial_text() {
        tokio::time::timeout(Duration::from_secs(3), async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}/stream", listener.local_addr().unwrap());
            let provider = async {
                let (stream, _) = listener.accept().await.unwrap();
                let mut first = accept_provider(stream).await;
                let start: Value = serde_json::from_str(first.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
                assert_eq!(start["type"], "session.start");
                first.send(Message::Text(json!({"type":"session.started"}).to_string().into())).await.unwrap();
                first.send(Message::Text(json!({"type":"transcript.final","utterance_id":"partial","revision":1,"text":"不完全"}).to_string().into())).await.unwrap();
                first.send(Message::Close(Some(CloseFrame { code: CloseCode::Error, reason: "".into() }))).await.unwrap();
                drop(first);
                let (stream, _) = listener.accept().await.unwrap();
                let socket = accept_provider(stream).await;
                provider_recording(socket, "AQD/fw==", vec![
                    json!({"type":"transcript.final","utterance_id":"full","revision":1,"text":"再接続後の全文"}),
                    json!({"type":"session.updated","session":{"status":"closed"}}),
                ], CloseCode::Normal).await;
            };
            let operation = async {
                let connection_url = url.clone();
                let prepared = Prepared::new("recording".into(), async move {
                    let token = Zeroizing::new("isolated-token".to_owned());
                    let socket = connect_stream(&token, "isolated-codex/1.0", &connection_url).await?;
                    Ok(PreparedConnection { token, socket })
                });
                // Stop can precede completion of the initial handshake.
                transcribe_authenticated("isolated-token", "isolated-codex/1.0",
                    &[1, 0, 255, 127], &url, "http://127.0.0.1:1/unused", Some(prepared)).await.unwrap()
            };
            let (text, ()) = tokio::join!(operation, provider);
            assert_eq!(text, "再接続後の全文");
        }).await.expect("prepared retry stalled");
    }

    #[tokio::test]
    async fn idle_connections_close_on_cancellation_expiry_and_remote_disconnect() {
        for end in ["cancel", "expire", "disconnect"] {
            let (client, server) = tokio::io::duplex(1024);
            let client = WebSocketStream::from_raw_socket(
                async_tungstenite::tokio::TokioAdapter::new(client),
                Role::Client,
                None,
            )
            .await;
            let mut server = WebSocketStream::from_raw_socket(
                async_tungstenite::tokio::TokioAdapter::new(server),
                Role::Server,
                None,
            )
            .await;
            let (recorded, receiver) = tokio::sync::oneshot::channel();
            let waiting = wait_for_recording(client, receiver, Duration::from_millis(20));
            let provider = async {
                server.send(Message::Ping(vec![1].into())).await.unwrap();
                assert_eq!(
                    server.next().await.unwrap().unwrap(),
                    Message::Pong(vec![1].into())
                );
                if end == "cancel" {
                    drop(recorded);
                } else {
                    let _keep_recording = recorded;
                    if end == "disconnect" {
                        server.close(None).await.unwrap();
                    }
                    std::future::pending::<()>().await;
                }
            };
            tokio::pin!(waiting, provider);
            let error = tokio::time::timeout(Duration::from_secs(1), async {
                tokio::select! {
                    result = &mut waiting => result.err().unwrap(),
                    () = &mut provider => waiting.await.err().unwrap(),
                }
            })
            .await
            .unwrap();
            assert!(error.contains(match end {
                "cancel" => "取り消",
                "expire" => "期限切れ",
                _ => "接続が切れ",
            }));
        }
    }

    #[tokio::test]
    async fn preparation_ownership_is_scoped_to_client_and_recording() {
        let dictation = Dictation::new(Err("isolated backend".into()));
        for session in [1, 2] {
            dictation.prepared.lock().unwrap().insert(
                session,
                Prepared::new(format!("recording-{session}"), std::future::pending()),
            );
        }
        dictation.cancel(1, "old-recording");
        assert!(dictation.take_preparation(2, Some("recording-1")).is_none());
        assert!(dictation.take_preparation(1, None).is_none());
        assert_eq!(dictation.prepared.lock().unwrap().len(), 2);
        dictation.cancel(1, "recording-1");
        assert_eq!(dictation.prepared.lock().unwrap().len(), 1);
        assert_eq!(
            dictation
                .take_preparation(2, Some("recording-2"))
                .unwrap()
                .id,
            "recording-2"
        );
        assert!(dictation.prepared.lock().unwrap().is_empty());
        dictation.prepared.lock().unwrap().insert(
            3,
            Prepared::new("last-recording".into(), std::future::pending()),
        );
        dictation.close_session(3);
        assert!(dictation.prepared.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn dropping_a_preparation_releases_its_live_socket() {
        tokio::time::timeout(Duration::from_secs(3), async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}/stream", listener.local_addr().unwrap());
            let (alive, warmed) = tokio::sync::oneshot::channel();
            let provider = async {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = accept_provider(stream).await;
                socket.send(Message::Ping(vec![9].into())).await.unwrap();
                assert_eq!(
                    socket.next().await.unwrap().unwrap(),
                    Message::Pong(vec![9].into())
                );
                alive.send(()).unwrap();
                assert!(
                    !matches!(socket.next().await, Some(Ok(_))),
                    "cancellation must drop the socket without starting recognition"
                );
            };
            let operation = async {
                let prepared = Prepared::new("cancelled".into(), async move {
                    let token = Zeroizing::new("isolated-token".to_owned());
                    let socket = connect_stream(&token, "isolated-codex/1.0", &url).await?;
                    Ok(PreparedConnection { token, socket })
                });
                warmed.await.unwrap();
                drop(prepared);
            };
            tokio::join!(operation, provider);
        })
        .await
        .expect("cancelled preparation leaked its socket");
    }

    struct CountFlushes<'a> {
        stream: tokio::io::DuplexStream,
        count: &'a Cell<usize>,
    }

    impl tokio::io::AsyncRead for CountFlushes<'_> {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buffer: &mut tokio::io::ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.stream).poll_read(cx, buffer)
        }
    }

    impl tokio::io::AsyncWrite for CountFlushes<'_> {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buffer: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Pin::new(&mut self.stream).poll_write(cx, buffer)
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            self.count.set(self.count.get() + 1);
            Pin::new(&mut self.stream).poll_flush(cx)
        }

        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.stream).poll_shutdown(cx)
        }
    }

    async fn provider_recording<S>(
        mut server: WebSocketStream<S>,
        audio: &str,
        events: Vec<Value>,
        close_code: CloseCode,
    ) where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let start: Value =
            serde_json::from_str(server.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(start["type"], "session.start");
        assert_eq!(start["config"]["input_audio_format"], "pcm16");
        assert_eq!(start["config"]["sample_rate_hz"], 24000);
        assert_eq!(start["config"]["num_channels"], 1);
        server
            .send(Message::Text(
                json!({"type":"session.started"}).to_string().into(),
            ))
            .await
            .unwrap();
        let mut received = Vec::new();
        loop {
            let message: Value =
                serde_json::from_str(server.next().await.unwrap().unwrap().to_text().unwrap())
                    .unwrap();
            if message["type"] == "session.close" {
                break;
            }
            assert_eq!(message["type"], "audio.append");
            let chunk = message["audio"].as_str().unwrap();
            assert!(chunk.len() <= AUDIO_CHUNK_BASE64_BYTES);
            let samples = STANDARD.decode(chunk).unwrap();
            assert_eq!(samples.len() % 2, 0);
            received.extend_from_slice(&samples);
        }
        assert_eq!(received, STANDARD.decode(audio).unwrap());
        for event in events {
            let closed =
                event["type"] == "session.updated" && event["session"]["status"] == "closed";
            server
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
            if closed {
                assert!(matches!(
                    server.next().await.unwrap().unwrap(),
                    Message::Close(_)
                ));
                return;
            }
        }
        server
            .send(Message::Close(Some(CloseFrame {
                code: close_code,
                reason: "".into(),
            })))
            .await
            .unwrap();
    }

    // The provider is the only test double. Both sides exercise the real
    // WebSocket implementation and dictation protocol, without account access.
    async fn recording_result(
        audio: &str,
        events: Vec<Value>,
        close_code: CloseCode,
    ) -> Result<String, String> {
        tokio::time::timeout(Duration::from_secs(3), async {
            let (client, server) = tokio::io::duplex(4096);
            let flushes = Cell::new(0);
            let client = CountFlushes {
                stream: client,
                count: &flushes,
            };
            let client = WebSocketStream::from_raw_socket(
                async_tungstenite::tokio::TokioAdapter::new(client),
                Role::Client,
                None,
            )
            .await;
            let server = WebSocketStream::from_raw_socket(
                async_tungstenite::tokio::TokioAdapter::new(server),
                Role::Server,
                None,
            )
            .await;
            let provider = provider_recording(server, audio, events, close_code);
            let (result, ()) = tokio::join!(transcribe_socket(client, audio), provider);
            let flushes = flushes.get();
            assert!(
                flushes <= 4 + audio.len() / AUDIO_CHUNK_BASE64_BYTES / 4,
                "upload must batch frames while allowing backpressure flushes: {flushes}"
            );
            result
        })
        .await
        .expect("dictation protocol stalled")
    }

    #[tokio::test]
    async fn joins_final_utterances_in_order_and_ignores_older_revisions() {
        let result = recording_result("AQD/fw==", vec![
            json!({"type":"transcript.final", "utterance_id":"first", "revision":2, "text":"こんにちは"}),
            json!({"type":"transcript.final", "utterance_id":"first", "revision":1, "text":"古い結果"}),
            json!({"type":"transcript.final", "utterance_id":"second", "revision":1, "text":"世界"}),
        ], CloseCode::Normal).await.unwrap();
        assert_eq!(result, "こんにちは 世界");
    }

    #[tokio::test]
    async fn rejects_failed_or_truncated_transcripts_instead_of_returning_partial_text() {
        let final_event = json!({"type":"transcript.final", "utterance_id":"first", "revision":1, "text":"途中まで"});
        for failure in ["transcript.failed", "session.error"] {
            assert!(
                recording_result(
                    "AQD/fw==",
                    vec![final_event.clone(), json!({"type":failure,"fatal":true})],
                    CloseCode::Normal
                )
                .await
                .is_err()
            );
        }
        assert!(
            recording_result("AQD/fw==", vec![final_event], CloseCode::Error)
                .await
                .is_err()
        );
        assert!(
            recording_result("AQD/fw==", vec![], CloseCode::Error)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn silence_completes_without_an_error() {
        for events in [
            vec![],
            vec![json!({"type":"session.updated", "session":{"status":"closed"}})],
            vec![
                json!({"type":"transcript.final", "utterance_id":"silent", "revision":1, "text":"  "}),
            ],
        ] {
            assert_eq!(
                recording_result("AAAAAA==", events, CloseCode::Normal)
                    .await
                    .unwrap(),
                ""
            );
        }
        assert_eq!(
            recording_fallback("200 OK", r#"{"text":"  "}"#, false, false, false)
                .await
                .unwrap()
                .trim(),
            ""
        );
        assert!(
            recording_fallback("200 OK", r#"{"text":42}"#, false, false, false)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn sends_a_full_recording_in_lossless_sample_aligned_chunks() {
        // More than the WebSocket write buffer and transport capacity, including
        // a partial last frame: batching must still drain under backpressure.
        let pcm: Vec<u8> = (0..(24_000 * 2 * 31 + 2))
            .map(|index| index as u8)
            .collect();
        let text = recording_result(&STANDARD.encode(pcm), vec![
            json!({"type":"transcript.final", "utterance_id":"first", "revision":1, "text":"長い録音"}),
        ], CloseCode::Normal).await.unwrap();
        assert_eq!(text, "長い録音");
    }

    #[tokio::test]
    async fn finishes_on_session_closed_and_keeps_transcripts_after_nonfatal_errors() {
        let text = recording_result("AQD/fw==", vec![
            json!({"type":"session.error", "fatal":false}),
            json!({"type":"transcript.final", "utterance_id":"first", "revision":1, "text":"完了"}),
            json!({"type":"session.updated", "session":{"status":"closed"}}),
        ], CloseCode::Error).await.unwrap();
        assert_eq!(text, "完了");
    }

    async fn recording_fallback(
        response_status: &str,
        response_body: &str,
        secure_stream: bool,
        api_key: bool,
        pending_preparation: bool,
    ) -> Result<String, String> {
        tokio::time::timeout(Duration::from_secs(if pending_preparation { 25 } else { 3 }), async {
            // Keep IPv4 port-discovery probes out of the isolated provider.
            let listener = tokio::net::TcpListener::bind("[::1]:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let token = format!("local.{}.signature", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
                br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"isolated-test-account"}}"#));
            let provider = async {
                for streaming in [true, false].into_iter().filter(|stream| !api_key || !stream) {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    if streaming && secure_stream {
                        // Reject TLS after ClientHello, before any credentials
                        // can reach the isolated provider.
                        assert_eq!(socket.read_u8().await.unwrap(), 0x16);
                        socket.shutdown().await.unwrap();
                        continue;
                    }
                    let mut request = Vec::new();
                    let mut buffer = [0_u8; 1024];
                    let header_end = loop {
                        let count = socket.read(&mut buffer).await.unwrap();
                        assert_ne!(count, 0);
                        request.extend_from_slice(&buffer[..count]);
                        assert!(request.len() < 16_384);
                        if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") { break end + 4; }
                    };
                    let headers = String::from_utf8(request[..header_end].to_vec()).unwrap().to_ascii_lowercase();
                    assert_eq!(headers.contains("user-agent: isolated-codex/1.0"), !api_key);
                    if streaming {
                        assert!(headers.starts_with("get /stream "));
                        assert!(headers.contains("upgrade: websocket"));
                        if pending_preparation {
                            // Leave the fresh handshake pending until the
                            // stream budget expires and the client closes it.
                            let _ = socket.read_to_end(&mut Vec::new()).await;
                        } else {
                            socket.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                        }
                        continue;
                    }
                    assert!(headers.starts_with("post /transcribe "));
                    assert!(headers.contains(&format!("authorization: bearer {}", token.to_ascii_lowercase())));
                    assert_eq!(headers.contains("chatgpt-account-id: isolated-test-account"), !api_key);
                    assert_eq!(headers.contains("originator: codex desktop"), !api_key);
                    assert!(headers.contains("content-type: multipart/form-data; boundary="));
                    let length: usize = headers.lines().find_map(|line| line.strip_prefix("content-length: ")).unwrap().parse().unwrap();
                    assert!(length < 8192);
                    let received = request.len();
                    request.resize(header_end + length, 0);
                    if received < header_end + length {
                        socket.read_exact(&mut request[received..]).await.unwrap();
                    }
                    let body = &request[header_end..];
                    assert_eq!(String::from_utf8_lossy(body).contains("gpt-4o-mini-transcribe"), api_key);
                    assert!(body.windows(16).any(|bytes| bytes == b"filename=\"codex."));
                    let wav_start = body.windows(4).position(|bytes| bytes == b"RIFF").unwrap();
                    let wav = &body[wav_start..wav_start + 48];
                    assert_eq!(&wav[8..16], b"WAVEfmt ");
                    assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 24_000);
                    assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 4);
                    assert_eq!(&wav[44..], &[1, 0, 255, 127]);
                    let response = format!("HTTP/1.1 {response_status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}", response_body.len());
                    socket.write_all(response.as_bytes()).await.unwrap();
                }
            };
            let scheme = if secure_stream { "wss" } else { "ws" };
            let stream_url = format!("{scheme}://{address}/stream");
            let recording_url = format!("http://{address}/transcribe");
            let operation = async {
                if api_key {
                    transcribe_recording(&token, RecordingService::OpenAi, &[1, 0, 255, 127], &recording_url).await
                } else {
                    let prepared = pending_preparation.then(|| Prepared::new("delayed".into(), std::future::pending()));
                    transcribe_authenticated(&token, "isolated-codex/1.0", &[1, 0, 255, 127], &stream_url, &recording_url, prepared).await
                }
            };
            let (result, ()) = tokio::join!(operation, provider);
            result
        }).await.expect("recording fallback stalled")
    }

    #[tokio::test]
    async fn api_key_recording_sends_model_without_chatgpt_headers_or_streaming() {
        assert_eq!(
            recording_fallback(
                "200 OK",
                r#"{"text":"API transcription"}"#,
                false,
                true,
                false
            )
            .await
            .unwrap(),
            "API transcription"
        );
        let error = recording_fallback(
            "401 Unauthorized",
            r#"{"error":"private provider payload"}"#,
            false,
            true,
            false,
        )
        .await
        .unwrap_err();
        assert!(error.contains("HTTP 401"));
        assert!(!error.contains("private provider payload"));
    }

    #[tokio::test]
    async fn submits_the_original_recording_when_streaming_is_rejected() {
        assert_eq!(
            recording_fallback(
                "200 OK",
                r#"{"text":"文字起こし成功"}"#,
                false,
                false,
                false
            )
            .await
            .unwrap(),
            "文字起こし成功"
        );
    }

    #[tokio::test]
    async fn slow_preparation_preserves_the_complete_recording_fallback_budget() {
        // Preparation takes ten seconds and the fresh handshake never replies.
        // Both must share the eighteen-second stream budget so the original
        // PCM can still reach file transcription within the overall 25 seconds.
        assert_eq!(
            recording_fallback(
                "200 OK",
                r#"{"text":"遅い接続でも全文"}"#,
                false,
                false,
                true
            )
            .await
            .unwrap(),
            "遅い接続でも全文"
        );
    }

    #[tokio::test]
    async fn reports_a_failed_recording_fallback_without_returning_provider_payloads() {
        let error = recording_fallback(
            "500 Internal Server Error",
            r#"{"error":"private provider payload"}"#,
            false,
            false,
            false,
        )
        .await
        .unwrap_err();
        assert!(error.contains("HTTP 403"));
        assert!(error.contains("HTTP 500"));
        assert!(!error.contains("private provider payload"));
    }

    #[tokio::test]
    async fn first_dictation_initializes_tls_for_both_auth_methods() {
        const CHILD: &str = "BEX_TEST_COLD_DICTATION_TLS";
        if std::env::var_os(CHILD).is_none() {
            // A separate process prevents another test from installing the
            // global TLS provider first and hiding cold-start failures.
            for method in ["api", "chatgpt"] {
                let output = tokio::time::timeout(
                Duration::from_secs(15),
                tokio::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "dictation::tests::first_dictation_initializes_tls_for_both_auth_methods",
                        "--nocapture",
                    ])
                    .env(CHILD, method)
                    .kill_on_drop(true)
                    .output(),
            )
            .await
            .expect("cold dictation process stalled")
            .unwrap();
                assert!(
                    output.status.success(),
                    "cold dictation failed: {}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            return;
        }
        let api_key = std::env::var(CHILD).unwrap() == "api";
        assert!(rustls::crypto::CryptoProvider::get_default().is_none());
        assert_eq!(
            recording_fallback(
                "200 OK",
                r#"{"text":"TLS後も文字起こし成功"}"#,
                !api_key,
                api_key,
                false
            )
            .await
            .unwrap(),
            "TLS後も文字起こし成功"
        );
    }
}
