//! Explicit error records, never request/response payloads or third-party traces.
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, value::RawValue};
use std::{
    cell::Cell,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{LazyLock, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

const FILE_BYTES: usize = 5 * 1024 * 1024;
const ARCHIVES: usize = 4;
const MESSAGE_BYTES: usize = 8192;
static LOG: OnceLock<Log> = OnceLock::new();
thread_local! {
    static WRITING: Cell<bool> = const { Cell::new(false) };
}

#[derive(Clone, Copy)]
pub enum Component {
    Desktop,
    Host,
}
impl Component {
    fn name(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Host => "host",
        }
    }
}

struct Log {
    path: PathBuf,
    component: &'static str,
    limit: usize,
    version: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Record<'a> {
    timestamp_ms: u128,
    pid: u32,
    component: &'static str,
    version: &'static str,
    revision: &'static str,
    level: &'static str,
    operation: &'a str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<Value>,
}

/// Call once before starting worker threads. Files are flushed for every record.
pub fn initialize(
    state_directory: &Path,
    component: Component,
    version: &'static str,
) -> io::Result<()> {
    let log = Log::open(
        &state_directory.join("logs"),
        component,
        FILE_BYTES,
        version,
    )?;
    log.write("info", "startup", "Bex started", None, None)?;
    LOG.set(log)
        .map_err(|_| io::Error::other("diagnostics already initialized"))?;
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        error("panic", &info.to_string());
        previous(info);
    }));
    Ok(())
}

pub fn error(operation: &str, message: &str) {
    record("error", operation, message, None, None);
}

pub fn shutdown() {
    record("info", "shutdown", "Bex shutting down", None, None);
}

pub fn rpc_error(operation: &str, request_id: Option<u64>, raw: &RawValue) {
    if LOG.get().is_none() {
        return;
    }
    #[derive(Deserialize)]
    struct RpcError {
        message: Option<String>,
        code: Option<Value>,
    }
    match serde_json::from_str::<RpcError>(raw.get()) {
        Ok(error) => record(
            "error",
            operation,
            error
                .message
                .as_deref()
                .unwrap_or("RPC error without a message"),
            request_id,
            error
                .code
                .filter(|code| code.is_number() || code.is_string()),
        ),
        Err(_) => match serde_json::from_str::<String>(raw.get()) {
            Ok(message) => record("error", operation, &message, request_id, None),
            Err(_) => record("error", operation, "Malformed RPC error", request_id, None),
        },
    }
}

/// Decode only error fields; ignore conversation bodies in turn notifications.
pub(crate) fn notification(message: &crate::peer::RpcMessage<'_>) {
    if LOG.get().is_none() {
        return;
    }
    let Some(method @ ("error" | "turn/completed" | "host/thread/watchFailed")) = message.method()
    else {
        return;
    };
    // This notification deliberately carries only watch/thread identity.
    if method == "host/thread/watchFailed" {
        error(method, "thread watch failed");
        return;
    }
    #[derive(Deserialize)]
    struct ErrorFields {
        error: Option<Box<RawValue>>,
        turn: Option<Box<ErrorFields>>,
    }
    if let Ok(params) = message.params::<ErrorFields>()
        && let Some(error) = params
            .error
            .or_else(|| params.turn.and_then(|turn| turn.error))
    {
        rpc_error(method, None, &error);
    }
}

fn record(
    level: &'static str,
    operation: &str,
    message: &str,
    id: Option<u64>,
    code: Option<Value>,
) {
    if let Some(log) = LOG.get()
        && let Err(error) = log.write(level, operation, message, id, code)
    {
        // Do not recursively try to log a full disk or an unwritable directory.
        let _ = writeln!(
            io::stderr().lock(),
            "Bex error log could not be written: {error}"
        );
    }
}

impl Log {
    fn open(
        directory: &Path,
        component: Component,
        limit: usize,
        version: &'static str,
    ) -> io::Result<Self> {
        fs::create_dir_all(directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            path: directory.join(format!("{}.jsonl", component.name())),
            component: component.name(),
            limit,
            version,
        })
    }

    fn write(
        &self,
        level: &'static str,
        operation: &str,
        message: &str,
        id: Option<u64>,
        code: Option<Value>,
    ) -> io::Result<()> {
        // A panic hook runs before unwinding releases the file lock. Never try
        // to acquire it again while this thread is already writing a record.
        if WRITING.replace(true) {
            return Err(io::Error::other("recursive diagnostic write"));
        }
        let _writing = scopeguard::guard((), |_| WRITING.set(false));
        let record = Record {
            timestamp_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            pid: std::process::id(),
            component: self.component,
            version: self.version,
            revision: option_env!("BEX_BUILD_REVISION").unwrap_or("development"),
            level,
            operation: &sanitize(operation),
            message: sanitize(message),
            request_id: id,
            error_code: code.map(|code| match code {
                Value::String(text) => Value::String(sanitize(&text)),
                other => other,
            }),
        };
        let mut bytes = serde_json::to_vec(&record)?;
        bytes.push(b'\n');
        let lock = private_options().open(self.path.with_extension("lock"))?;
        lock.lock()?;
        let mut writer = private_options().open(&self.path)?;
        if writer.metadata()?.len() > self.limit as u64 {
            drop(writer);
            let archive = |index| {
                let mut path = self.path.as_os_str().to_owned();
                path.push(format!(".{index}"));
                PathBuf::from(path)
            };
            for index in (1..ARCHIVES).rev() {
                match fs::rename(archive(index), archive(index + 1)) {
                    Ok(()) => (),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                    Err(error) => return Err(error),
                }
            }
            fs::rename(&self.path, archive(1))?;
            writer = private_options().open(&self.path)?;
        }
        writer.write_all(&bytes)?;
        writer.flush()
    }
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// Preserve diagnostic prose while removing structured payloads and credentials.
pub fn sanitize(message: &str) -> String {
    static SENSITIVE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
        r#"(?i)(?:bearer\s+[a-z0-9._~+/=-]+|\bsk-[a-z0-9_-]+|\beyJ[a-z0-9_-]+\.[a-z0-9_-]+\.[a-z0-9_-]+|(?:authorization|access[_-]?token|refresh[_-]?token|api[_-]?key|password|client[_-]?secret|cookie|ticket)["']?\s*[:=]\s*(?:"[^"]*"|'[^']*'|[^\s,;}]+))"#
    ).unwrap()
    });
    static URLS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https?://[^\s<>"']+"#).unwrap());
    static HOMES: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?:/Users/|/home/|[A-Za-z]:\\Users\\)[^/\\\s]+").unwrap());
    static EMAIL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}").unwrap());
    static PAYLOAD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)\{.*\}|data:[^,\s]*;base64,[A-Za-z0-9+/=]+").unwrap());
    let value = message
        .strip_prefix("remote RPC error: ")
        .unwrap_or(message);
    let envelope = serde_json::from_str::<Value>(value).ok();
    let message = envelope
        .as_ref()
        .and_then(|v| v.get("message"))
        .and_then(Value::as_str)
        .unwrap_or(message);
    let clean = URLS.replace_all(message, |caps: &regex::Captures<'_>| match url::Url::parse(
        &caps[0],
    ) {
        Ok(mut url) => {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.set_fragment(None);
            url.to_string()
        }
        Err(_) => "[URL omitted]".into(),
    });
    let clean = SENSITIVE.replace_all(&clean, "[credential omitted]");
    let clean = PAYLOAD.replace_all(&clean, "[payload omitted]");
    let clean = HOMES.replace_all(&clean, regex::NoExpand("$HOME"));
    let mut clean = EMAIL.replace_all(&clean, "[email omitted]").into_owned();
    if clean.len() > MESSAGE_BYTES {
        let mut end = MESSAGE_BYTES;
        while !clean.is_char_boundary(end) {
            end -= 1;
        }
        clean.truncate(end);
        clean.push_str(" [truncated]");
    }
    clean
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_rpc_cause_without_structured_payloads_or_credentials() {
        let raw = r#"remote RPC error: {"code":-32603,"message":"failed to read session metadata /Users/example/.codex/session.jsonl is empty","data":{"prompt":"PRIVATE_PROMPT"}}"#;
        let message = sanitize(raw);
        assert_eq!(
            message,
            "failed to read session metadata $HOME/.codex/session.jsonl is empty"
        );
        let message = sanitize(
            "Bearer SECRET_TOKEN password='SECRET_PASSWORD' https://user:SECRET_PASSWORD@example.com/api?token=SECRET_TOKEN#SECRET_TOKEN user@example.com data:image/png;base64,U0VDUkVU {\"text\":\"PRIVATE_PROMPT\"}",
        );
        for secret in ["SECRET", "PRIVATE_PROMPT", "user@", "U0VDUkVU"] {
            assert!(!message.contains(secret), "{message}");
        }
        assert!(message.contains("https://example.com/api"));
        assert!(sanitize(&"あ".repeat(MESSAGE_BYTES)).ends_with(" [truncated]"));
    }

    #[test]
    fn reopens_and_rotates_complete_private_records_with_bounded_retention() {
        let directory = tempfile::tempdir().unwrap();
        let log = Log::open(directory.path(), Component::Host, 1024, "test-version").unwrap();
        for index in 0..50 {
            // Independent writers model app restarts sharing the same directory.
            let writer =
                Log::open(directory.path(), Component::Host, 1024, "test-version").unwrap();
            writer
                .write(
                    "error",
                    "thread/read",
                    &format!("failure {index}"),
                    Some(index),
                    Some(Value::from(-32603)),
                )
                .unwrap();
        }
        let mut ids = Vec::new();
        let mut files = 0;
        for entry in fs::read_dir(directory.path()).unwrap() {
            let path = entry.unwrap().path();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
            if path.extension().is_some_and(|ext| ext == "lock") {
                continue;
            }
            files += 1;
            for line in fs::read_to_string(path).unwrap().lines() {
                let record: Value = serde_json::from_str(line).unwrap();
                assert_eq!(record["operation"], "thread/read");
                assert_eq!(record["errorCode"], -32603);
                assert_eq!(record["component"], "host");
                assert_eq!(record["version"], "test-version");
                ids.push(record["requestId"].as_u64().unwrap());
            }
        }
        assert!(files <= ARCHIVES + 1);
        assert!(ids.contains(&49));
        assert!(!ids.contains(&0));
        log.write("info", "shutdown", "Bex stopped", None, None)
            .unwrap();
        assert!(fs::read_to_string(&log.path).unwrap().contains("shutdown"));
    }

    #[test]
    fn rotation_failure_preserves_current_records_and_recovers() {
        let directory = tempfile::tempdir().unwrap();
        let log = Log::open(directory.path(), Component::Host, 1, "test-version").unwrap();
        log.write("error", "first", "first record", None, None)
            .unwrap();
        let mut archive = log.path.as_os_str().to_owned();
        archive.push(".4");
        let archive = PathBuf::from(archive);
        fs::create_dir(&archive).unwrap();
        let mut prior = log.path.as_os_str().to_owned();
        prior.push(".3");
        fs::write(prior, "older archive").unwrap();
        assert!(
            log.write("error", "second", "second record", None, None)
                .is_err()
        );
        assert!(
            fs::read_to_string(&log.path)
                .unwrap()
                .contains("first record")
        );
        for entry in fs::read_dir(directory.path()).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                fs::remove_dir(path).unwrap();
            }
        }
        log.write("error", "recovered", "recovered record", None, None)
            .unwrap();
        assert!(
            fs::read_to_string(&log.path)
                .unwrap()
                .contains("recovered record")
        );
    }

    #[cfg(not(unix))]
    #[test]
    fn unwritable_destination_returns_an_error_and_can_recover() {
        let directory = tempfile::tempdir().unwrap();
        let log = Log::open(
            directory.path(),
            Component::Desktop,
            FILE_BYTES,
            "test-version",
        )
        .unwrap();
        fs::create_dir(&log.path).unwrap();
        assert!(log.write("error", "test", "failure", None, None).is_err());
        fs::remove_dir(&log.path).unwrap();
        log.write("error", "test", "recovered", None, None).unwrap();
        assert!(fs::read_to_string(log.path).unwrap().contains("recovered"));
    }
}
