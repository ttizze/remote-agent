//! OpenCode Go reports used percentages; the account UI displays the remainder.
use agent_protocol::operations::UsageWindow;
use serde_json::Value;
use std::time::Duration;

const OPENCODE_GO_USAGE: &str = "https://opencode.ai/inference/go/v1/usage";
const WINDOWS: [(&str, &str); 3] = [
    ("rolling", "OpenCode Go · 5時間枠"),
    ("weekly", "OpenCode Go · 週間枠"),
    ("monthly", "OpenCode Go · 月間枠"),
];

pub(super) async fn read(key: &str) -> Result<Vec<UsageWindow>, String> {
    read_url(OPENCODE_GO_USAGE, key).await
}

async fn read_url(url: &str, key: &str) -> Result<Vec<UsageWindow>, String> {
    // A cold Host can link both TLS backends without a selected default.
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    let response = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("BEX/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|_| "OpenCodeの残量取得を初期化できませんでした。".to_string())?
        .get(url)
        .bearer_auth(key)
        .header("accept", "application/json")
        .send()
        .await
        .map_err(|_| "OpenCodeの残量を取得できませんでした。".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "OpenCodeの残量取得に失敗しました（HTTP {}）。",
            response.status().as_u16()
        ));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|_| "OpenCodeの残量応答を読み取れませんでした。".to_string())?;
    Ok(windows(&body))
}

fn windows(value: &Value) -> Vec<UsageWindow> {
    WINDOWS
        .iter()
        .filter_map(|(field, label)| {
            let window = value.get("usage")?.get(*field)?;
            let resets_at = window
                .get("resetsAt")
                .and_then(Value::as_str)
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .map(|instant| instant.timestamp());
            UsageWindow::from_used((*label).into(), window.get("percent")?.as_f64()?, resets_at)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn subscription_windows_show_remaining_allowance_and_reset_times() {
        assert_eq!(
            windows(&json!({"usage":{
                "rolling":{"status":"ok","percent":42,"resetsAt":"2000-01-01T00:00:00.000Z"},
                "weekly":{"status":"ok","percent":18,"resetsAt":"2000-01-08T00:00:00Z"},
                "monthly":{"status":"rate-limited","percent":100,"resetsAt":"invalid"}
            }})),
            vec![
                UsageWindow {
                    label: "OpenCode Go · 5時間枠".into(),
                    remaining_percent: 58,
                    resets_at: Some(946684800)
                },
                UsageWindow {
                    label: "OpenCode Go · 週間枠".into(),
                    remaining_percent: 82,
                    resets_at: Some(947289600)
                },
                UsageWindow {
                    label: "OpenCode Go · 月間枠".into(),
                    remaining_percent: 0,
                    resets_at: None
                },
            ]
        );
    }

    #[test]
    fn unknown_measurements_never_become_full_allowance() {
        for value in [
            json!({"usage":null}),
            json!({}),
            json!({"usage":{"rolling":{"percent":"42"},"weekly":{}}}),
        ] {
            assert!(windows(&value).is_empty());
        }
        let windows = windows(
            &json!({"usage":{"rolling":{"percent":120},"weekly":{"percent":-4},"monthly":{"percent":12.5}}}),
        );
        assert_eq!(
            windows
                .iter()
                .map(|window| window.remaining_percent)
                .collect::<Vec<_>>(),
            [0, 100, 87]
        );
    }

    #[tokio::test]
    async fn requests_identify_the_client_and_do_not_forward_keys_or_error_bodies() {
        for status in [200, 307, 401, 403, 503] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let count = stream.read(&mut buffer).await.unwrap();
                    assert!(count > 0 && request.len() < 8192);
                    request.extend_from_slice(&buffer[..count]);
                }
                let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
                assert!(request.starts_with("get /usage http/1.1\r\n"));
                assert!(request.contains("\r\nauthorization: bearer test-credential\r\n"));
                assert!(request.contains(concat!(
                    "\r\nuser-agent: bex/",
                    env!("CARGO_PKG_VERSION"),
                    "\r\n"
                )));
                let body = if status == 200 {
                    r#"{"usage":{"weekly":{"percent":25}}}"#
                } else {
                    "untrusted diagnostic test-credential"
                };
                let response = format!(
                    "HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nLocation: http://127.0.0.1:1/not-followed\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            });
            let result = read_url(&format!("http://{address}/usage"), "test-credential").await;
            server.await.unwrap();
            if status == 200 {
                let windows = result.unwrap();
                assert_eq!(windows.len(), 1);
                assert_eq!(windows[0].remaining_percent, 75);
            } else {
                let error = result.unwrap_err();
                assert!(error.contains(&format!("HTTP {status}")));
                assert!(!error.contains("test-credential") && !error.contains("untrusted"));
            }
        }
    }
}
