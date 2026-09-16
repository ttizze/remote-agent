use std::{sync::OnceLock, time::Duration};

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
const TRANSCRIBE_URL: &str = "https://chatgpt.com/backend-api/transcribe";

pub(crate) struct Dictation {
    backend: Result<std::sync::Arc<CodexAppServer>, String>,
}
impl Dictation {
    pub(crate) fn new(backend: Result<std::sync::Arc<CodexAppServer>, String>) -> Self {
        Self { backend }
    }
    pub(crate) async fn transcribe(
        &self,
        params: &agent_core::client::Transcribe<&str>,
    ) -> Result<agent_core::client::Transcription, String> {
        let app_server = self
            .backend
            .as_deref()
            .map_err(|error| format!("音声入力のCodexバックエンドを利用できません: {error}"))?;
        tokio::time::timeout(
            Duration::from_secs(25),
            transcribe_request(app_server, params),
        )
        .await
        .map_err(|_| "文字起こしがタイムアウトしました。")?
    }
}

async fn transcribe_request(
    app_server: &CodexAppServer,
    params: &agent_core::client::Transcribe<&str>,
) -> Result<agent_core::client::Transcription, String> {
    let audio = params.audio;
    let pcm = Zeroizing::new(
        STANDARD
            .decode(audio)
            .map_err(|_| "録音データが無効です。")?,
    );
    if pcm.is_empty() || pcm.len() % 2 != 0 {
        return Err("録音データが無効です。".into());
    }

    // Keep the desktop account token on the Host. Neither the RPC response nor
    // an error contains the token or the authenticated WebSocket request.
    let token = crate::codex_accounts::access_token(app_server, false).await?;
    let text = transcribe_authenticated(
        &token,
        &app_server.initialize_response().user_agent,
        audio,
        &pcm,
        DICTATION_URL,
        TRANSCRIBE_URL,
    )
    .await?;
    Ok(agent_core::client::Transcription {
        text,
        extra: Default::default(),
    })
}

async fn transcribe_authenticated(
    token: &str,
    user_agent: &str,
    audio: &str,
    pcm: &[u8],
    stream_url: &str,
    recording_url: &str,
) -> Result<String, String> {
    // The desktop build enables both Rustls backends. Choose before the first
    // WebSocket handshake; iroh configures its own provider without installing
    // a process default. Preserve a provider already selected by another caller.
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    // The desktop retains a recording alongside its stream and submits that
    // recording to /transcribe when streaming cannot produce a transcript.
    let stream_result = tokio::time::timeout(
        Duration::from_secs(18),
        transcribe_stream(token, user_agent, audio, stream_url),
    )
    .await
    .unwrap_or_else(|_| Err("音声ストリームがタイムアウトしました。".into()));
    match stream_result {
        Ok(text) => Ok(text),
        Err(stream_error) => transcribe_recording(token, user_agent, pcm, recording_url)
            .await
            .map_err(|error| format!("{stream_error}\n録音ファイルの文字起こし: {error}")),
    }
}

async fn transcribe_stream(
    token: &str,
    user_agent: &str,
    audio: &str,
    url: &str,
) -> Result<String, String> {
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
    transcribe_socket(socket, audio).await
}

async fn transcribe_recording(
    token: &str,
    user_agent: &str,
    pcm: &[u8],
    url: &str,
) -> Result<String, String> {
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

    // The phone provides raw PCM rather than the desktop's MediaRecorder Blob.
    // A WAV container preserves those samples and declares their capture format.
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
    let mut request = client
        .post(url)
        .bearer_auth(token)
        .header("originator", "Codex Desktop")
        // Use the running App Server's own identity, as the native Codex CLI
        // does for transcription, rather than inventing a browser identity.
        .header("User-Agent", user_agent)
        .multipart(reqwest::multipart::Form::new().part("file", file));

    // As in the desktop's authenticated fetch, route to the token's account.
    // These unverified claims only select headers; the service verifies the JWT.
    if let Some(claims) = crate::codex_accounts::token_claims(token) {
        let auth = &claims["https://api.openai.com/auth"];
        if let Some(account_id) = auth["chatgpt_account_id"].as_str() {
            let mut account = HeaderValue::from_str(account_id)
                .map_err(|_| "Codexのアカウント情報が無効です。")?;
            account.set_sensitive(true);
            request = request.header("ChatGPT-Account-Id", account);
        }
    }
    let response = request
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
                        // The caller has validated this as base64 ASCII. Send
                        // small frames, as the desktop's streaming capture does.
                        for start in (0..audio.len()).step_by(AUDIO_CHUNK_BASE64_BYTES) {
                            let audio = &audio[start..(start + AUDIO_CHUNK_BASE64_BYTES).min(audio.len())];
                            socket.send(Message::Text(serde_json::to_string(&AudioAppend { kind: "audio.append", audio })
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // The provider is the only test double. Both sides exercise the real
    // WebSocket implementation and dictation protocol, without account access.
    async fn recording_result(
        audio: &str,
        events: Vec<Value>,
        close_code: CloseCode,
    ) -> Result<String, String> {
        tokio::time::timeout(Duration::from_secs(3), async {
            let (client, server) = tokio::io::duplex(4096);
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
            let provider = async {
                let start: Value =
                    serde_json::from_str(server.next().await.unwrap().unwrap().to_text().unwrap())
                        .unwrap();
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
                    let message: Value = serde_json::from_str(
                        server.next().await.unwrap().unwrap().to_text().unwrap(),
                    )
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
                    let closed = event["type"] == "session.updated"
                        && event["session"]["status"] == "closed";
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
            };
            let (result, ()) = tokio::join!(transcribe_socket(client, audio), provider);
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
            recording_fallback("200 OK", r#"{"text":"  "}"#, false)
                .await
                .unwrap()
                .trim(),
            ""
        );
        assert!(
            recording_fallback("200 OK", r#"{"text":42}"#, false)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn sends_a_full_recording_in_lossless_sample_aligned_chunks() {
        let pcm: Vec<u8> = (0..(AUDIO_CHUNK_BASE64_BYTES / 4 * 3 * 2 + 2))
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
    ) -> Result<String, String> {
        tokio::time::timeout(Duration::from_secs(3), async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let token = format!("local.{}.signature", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
                br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"isolated-test-account"}}"#));
            let provider = async {
                for streaming in [true, false] {
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
                    assert!(headers.contains("user-agent: isolated-codex/1.0"));
                    if streaming {
                        assert!(headers.starts_with("get /stream "));
                        assert!(headers.contains("upgrade: websocket"));
                        socket.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                        continue;
                    }
                    assert!(headers.starts_with("post /transcribe "));
                    assert!(headers.contains(&format!("authorization: bearer {}", token.to_ascii_lowercase())));
                    assert!(headers.contains("chatgpt-account-id: isolated-test-account"));
                    assert!(headers.contains("originator: codex desktop"));
                    assert!(headers.contains("content-type: multipart/form-data; boundary="));
                    let length: usize = headers.lines().find_map(|line| line.strip_prefix("content-length: ")).unwrap().parse().unwrap();
                    assert!(length < 8192);
                    let received = request.len();
                    request.resize(header_end + length, 0);
                    if received < header_end + length {
                        socket.read_exact(&mut request[received..]).await.unwrap();
                    }
                    let body = &request[header_end..];
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
            let operation = transcribe_authenticated(&token, "isolated-codex/1.0", "AQD/fw==", &[1, 0, 255, 127], &stream_url, &recording_url);
            let (result, ()) = tokio::join!(operation, provider);
            result
        }).await.expect("recording fallback stalled")
    }

    #[tokio::test]
    async fn submits_the_original_recording_when_streaming_is_rejected() {
        assert_eq!(
            recording_fallback("200 OK", r#"{"text":"文字起こし成功"}"#, false)
                .await
                .unwrap(),
            "文字起こし成功"
        );
    }

    #[tokio::test]
    async fn reports_a_failed_recording_fallback_without_returning_provider_payloads() {
        let error = recording_fallback(
            "500 Internal Server Error",
            r#"{"error":"private provider payload"}"#,
            false,
        )
        .await
        .unwrap_err();
        assert!(error.contains("HTTP 403"));
        assert!(error.contains("HTTP 500"));
        assert!(!error.contains("private provider payload"));
    }

    #[tokio::test]
    async fn first_secure_dictation_falls_back_after_tls_failure() {
        const CHILD: &str = "BEX_TEST_COLD_DICTATION_TLS";
        if std::env::var_os(CHILD).is_none() {
            // A separate process prevents another test from installing the
            // global TLS provider first and hiding cold-start failures.
            let output = tokio::time::timeout(
                Duration::from_secs(15),
                tokio::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "dictation::tests::first_secure_dictation_falls_back_after_tls_failure",
                        "--nocapture",
                    ])
                    .env(CHILD, "1")
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
            return;
        }
        assert!(rustls::crypto::CryptoProvider::get_default().is_none());
        assert_eq!(
            recording_fallback("200 OK", r#"{"text":"TLS後も文字起こし成功"}"#, true)
                .await
                .unwrap(),
            "TLS後も文字起こし成功"
        );
    }
}
