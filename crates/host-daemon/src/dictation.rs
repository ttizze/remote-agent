use std::time::Duration;

use base64::{Engine, engine::general_purpose::STANDARD};
use codex_app_server::CodexAppServer;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::{WebSocketStream, tungstenite::{Message, client::IntoClientRequest, http::HeaderValue}};
use zeroize::Zeroizing;

// Matches the PCM16, mono capture format on the phone. Limit one recording to
// 30 seconds; the dictation service also bounds individual utterances to 30s.
const MAX_AUDIO_BYTES: usize = 24_000 * 2 * 30;
// 100ms of PCM16 at 24kHz. Base64 and sample boundaries both remain aligned.
const AUDIO_CHUNK_BASE64_BYTES: usize = 6_400;
const DICTATION_URL: &str = "wss://chatgpt.com/backend-api/dictation/stream";

pub(crate) async fn transcribe(app_server: &CodexAppServer, params: &Value) -> Result<Value, String> {
    tokio::time::timeout(Duration::from_secs(25), transcribe_request(app_server, params))
        .await.map_err(|_| "文字起こしがタイムアウトしました。")?
}

async fn transcribe_request(app_server: &CodexAppServer, params: &Value) -> Result<Value, String> {
    let audio = params.get("audio").and_then(Value::as_str).ok_or("録音データがありません。")?;
    if audio.is_empty() || audio.len() > MAX_AUDIO_BYTES.div_ceil(3) * 4 {
        return Err("音声は30秒以内で録音してください。".into());
    }
    let pcm = Zeroizing::new(STANDARD.decode(audio).map_err(|_| "録音データが無効です。")?);
    if pcm.is_empty() || pcm.len() > MAX_AUDIO_BYTES || pcm.len() % 2 != 0 {
        return Err("録音データが無効です。".into());
    }
    drop(pcm);

    // Keep the desktop account token on the Host. Neither the RPC response nor
    // an error contains the token or the authenticated WebSocket request.
    let response = Zeroizing::new(app_server.request_raw(
        r#"{"id":"host-dictation-auth","method":"getAuthStatus","params":{"includeToken":true,"refreshToken":false}}"#,
    ).await.map_err(|_| "Codexの認証情報を取得できませんでした。")?);
    let mut response: Value = serde_json::from_str(&response).map_err(|_| "Codexの認証応答が無効です。")?;
    let result = &mut response["result"];
    if !matches!(result["authMethod"].as_str(), Some("chatgpt" | "chatgptAuthTokens")) {
        return Err("MacのCodexにChatGPTアカウントでログインしてください。".into());
    }
    let Value::String(token) = result["authToken"].take() else {
        return Err("MacのCodexにChatGPTアカウントでログインしてください。".into());
    };
    let token = Zeroizing::new(token);
    let mut request = DICTATION_URL.into_client_request().map_err(|_| "音声処理の接続先が無効です。")?;
    let protocols = Zeroizing::new(format!("chatgpt-dictation, openai-bearer.{}, codex-desktop", token.as_str()));
    let mut header = HeaderValue::from_str(&protocols).map_err(|_| "Codexの認証情報が無効です。")?;
    header.set_sensitive(true);
    request.headers_mut().insert("Sec-WebSocket-Protocol", header);
    let (socket, _) = tokio_tungstenite::connect_async(request).await
        .map_err(|error| match error {
            tokio_tungstenite::tungstenite::Error::Http(response)
                if response.headers().get("cf-mitigated").is_some_and(|value| value == "challenge") =>
                "Codexの音声サービスがブラウザでの確認を要求しているため、接続できません。".into(),
            tokio_tungstenite::tungstenite::Error::Http(response) =>
                format!("Codexの音声処理に接続できませんでした（HTTP {}）。", response.status().as_u16()),
            tokio_tungstenite::tungstenite::Error::Tls(_) => "Codexの音声処理とのTLS接続に失敗しました。".into(),
            tokio_tungstenite::tungstenite::Error::Io(error) =>
                format!("Codexの音声処理に接続できませんでした（通信エラー: {:?}）。", error.kind()),
            _ => "Codexの音声処理とのWebSocket接続に失敗しました。".into(),
        })?;
    let text = transcribe_socket(socket, audio).await?;
    Ok(json!({ "text": text }))
}

#[derive(Serialize)]
struct AudioAppend<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    audio: &'a str,
}

async fn transcribe_socket<S>(mut socket: WebSocketStream<S>, audio: &str) -> Result<String, String>
where S: AsyncRead + AsyncWrite + Unpin {
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
                    Some("transcript.failed" | "session.error") => return Err("Codexで文字起こしできませんでした。".into()),
                    _ => {}
                }
            }
            Message::Ping(_) => socket.flush().await.map_err(|_| "音声処理との接続が切れました。")?,
            Message::Close(frame) if started && frame.as_ref().is_some_and(|frame| frame.code == tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Normal) => {
                let mut text = String::new();
                for (_, _, segment) in transcripts {
                    let segment = segment.trim();
                    if segment.is_empty() { continue; }
                    if !text.is_empty() { text.push(' '); }
                    text.push_str(segment);
                }
                if text.is_empty() { return Err("音声を認識できませんでした。もう一度録音してください。".into()); }
                return Ok(text);
            }
            Message::Close(_) => return Err("文字起こしが完了する前に接続が切れました。".into()),
            _ => {}
        }
    }
    Err("文字起こしが完了する前に接続が切れました。".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role, frame::coding::CloseCode};

    // The provider is the only test double. Both sides exercise the real
    // WebSocket implementation and dictation protocol, without account access.
    async fn recording_result(audio: &str, events: Vec<Value>, close_code: CloseCode) -> Result<String, String> {
        tokio::time::timeout(Duration::from_secs(3), async {
            let (client, server) = tokio::io::duplex(4096);
            let client = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
            let mut server = WebSocketStream::from_raw_socket(server, Role::Server, None).await;
            let provider = async {
                let start: Value = serde_json::from_str(server.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
                assert_eq!(start["type"], "session.start");
                assert_eq!(start["config"]["input_audio_format"], "pcm16");
                assert_eq!(start["config"]["sample_rate_hz"], 24000);
                assert_eq!(start["config"]["num_channels"], 1);
                server.send(Message::Text(json!({"type":"session.started"}).to_string().into())).await.unwrap();
                let mut received = Vec::new();
                loop {
                    let message: Value = serde_json::from_str(server.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
                    if message["type"] == "session.close" { break; }
                    assert_eq!(message["type"], "audio.append");
                    let chunk = message["audio"].as_str().unwrap();
                    assert!(chunk.len() <= AUDIO_CHUNK_BASE64_BYTES);
                    let samples = STANDARD.decode(chunk).unwrap();
                    assert_eq!(samples.len() % 2, 0);
                    received.extend_from_slice(&samples);
                }
                assert_eq!(received, STANDARD.decode(audio).unwrap());
                for event in events { server.send(Message::Text(event.to_string().into())).await.unwrap(); }
                server.send(Message::Close(Some(CloseFrame { code: close_code, reason: "".into() }))).await.unwrap();
            };
            let (result, ()) = tokio::join!(transcribe_socket(client, audio), provider);
            result
        }).await.expect("dictation protocol stalled")
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
            assert!(recording_result("AQD/fw==", vec![final_event.clone(), json!({"type":failure})], CloseCode::Normal).await.is_err());
        }
        assert!(recording_result("AQD/fw==", vec![final_event], CloseCode::Error).await.is_err());
        assert!(recording_result("AQD/fw==", vec![], CloseCode::Normal).await.is_err());
    }

    #[tokio::test]
    async fn sends_a_full_recording_in_lossless_sample_aligned_chunks() {
        let pcm: Vec<u8> = (0..MAX_AUDIO_BYTES).map(|index| index as u8).collect();
        let text = recording_result(&STANDARD.encode(pcm), vec![
            json!({"type":"transcript.final", "utterance_id":"first", "revision":1, "text":"長い録音"}),
        ], CloseCode::Normal).await.unwrap();
        assert_eq!(text, "長い録音");
    }
}
