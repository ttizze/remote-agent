//! Bounded one-shot CLI inference. Never persist prompts or log provider output.
use super::Failure;
use std::{process::Stdio, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt};

pub(super) fn schema() -> serde_json::Value {
    serde_json::json!({"type":"object","properties":{"title":{"type":"string"}},"required":["title"],"additionalProperties":false})
}

pub(super) fn title(output: &str) -> Result<String, Failure> {
    let value: serde_json::Value = serde_json::from_str(output)
        .map_err(|_| Failure::new("title_generation_failed", "Invalid title output"))?;
    value["title"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Failure::new("title_generation_failed", "Title output has no title"))
}

async fn read(pipe: impl AsyncRead + Unpin) -> Result<Vec<u8>, std::io::Error> {
    let mut bytes = Vec::new();
    pipe.take(256 * 1024 + 1).read_to_end(&mut bytes).await?;
    if bytes.len() > 256 * 1024 {
        return Err(std::io::Error::other("Title output exceeds limit"));
    }
    Ok(bytes)
}

pub(super) async fn run(
    mut command: tokio::process::Command,
    prompt: &str,
) -> Result<String, Failure> {
    let operation = async {
        // The supervisor's stdin owns the whole process tree. A one-shot CLI
        // gets its bounded prompt as an argument so EOF cannot kill inference.
        let mut child = command
            .arg(prompt)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| "Could not start title generator")?;
        let lifetime = child
            .stdin
            .take()
            .ok_or("Title lifetime pipe is unavailable")?;
        let stdout = child.stdout.take().ok_or("Title output is unavailable")?;
        let stderr = child
            .stderr
            .take()
            .ok_or("Title diagnostics are unavailable")?;
        let (stdout, _, status) = tokio::try_join!(read(stdout), read(stderr), child.wait())
            .map_err(|_| "Title generator I/O failed")?;
        drop(lifetime);
        if !status.success() {
            return Err("Title generator failed");
        }
        String::from_utf8(stdout).map_err(|_| "Title output is not UTF-8")
    };
    tokio::time::timeout(Duration::from_secs(180), operation)
        .await
        .map_err(|_| Failure::new("title_generation_failed", "Title generation timed out"))?
        .map_err(|message| Failure::new("title_generation_failed", message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn title_output_is_bounded_and_invalid_structured_results_are_rejected() {
        assert_eq!(
            title(r#"{"title":"日本語の名前"}"#).unwrap(),
            "日本語の名前"
        );
        for output in ["not JSON", "{}", r#"{"title":42}"#] {
            assert!(title(output).is_err());
        }
        let bytes = vec![b'x'; 256 * 1024 + 1];
        assert!(read(bytes.as_slice()).await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn title_errors_never_include_provider_output_or_prompt_text() {
        let mut command = agent_process::command(std::path::Path::new("/bin/sh")).unwrap();
        command.args(["-c", "echo private-provider-diagnostic >&2; exit 7"]);
        let error = run(command, "private-prompt").await.unwrap_err();
        assert_eq!(error.message, "Title generator failed");
    }
}
