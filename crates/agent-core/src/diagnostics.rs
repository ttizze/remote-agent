//! Explicit diagnostic fields, never arbitrary payloads or third-party traces.
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, value::RawValue};
use std::{
    cell::Cell,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::LazyLock,
    time::{SystemTime, UNIX_EPOCH},
};
use tracing_subscriber::{Layer, layer::SubscriberExt};

pub mod connection;
pub use connection::{ConnectionPhase, ConnectionTimeline};

const FILE_BYTES: usize = 5 * 1024 * 1024;
const ARCHIVES: usize = 4;
const MESSAGE_BYTES: usize = 8192;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub enum ConnectionRoute {
    Direct,
    Relay,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub enum ClientPlatform {
    Ios,
    Android,
    Macos,
    #[default]
    Other,
}

/// Fixed categories and durations only: no identifiers or conversation content.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConnectionPerformance {
    pub total_ms: u64,
    pub endpoint_ms: u64,
    pub transport_ms: u64,
    pub verification_ms: u64,
    pub reused: bool,
    pub route: ConnectionRoute,
    pub platform: ClientPlatform,
    pub resolution_ms: u64,
    pub rtt_ms: u64,
    pub connection_id: u64,
    pub attempt_id: u64,
    pub report: u8,
    pub client_revision: String,
    pub timeline: ConnectionTimeline,
}

pub fn connection_performance(performance: &ConnectionPerformance) {
    tracing::info!(target: "bex", operation = "client.connection",
    message = %format_args!(
        "platform={:?} total_ms={} endpoint_ms={} transport_ms={} verification_ms={} reused={} route={:?} resolution_ms={} rtt_ms={}",
        performance.platform, performance.total_ms, performance.endpoint_ms,
        performance.transport_ms, performance.verification_ms, performance.reused, performance.route,
        performance.resolution_ms, performance.rtt_ms,
    ));
    if performance.timeline.id != 0 {
        tracing::info!(target: "bex", operation = "client.connection.timeline",
            message = %format_args!("trace={} attempt={} connection={} report={} dropped={} platform={:?} client_revision={}",
                performance.timeline.id, performance.attempt_id, performance.connection_id,
                performance.report, performance.timeline.dropped, performance.platform,
                performance.client_revision.chars().filter(char::is_ascii_alphanumeric).take(40).collect::<String>()));
        for event in performance.timeline.events.iter().take(768) {
            tracing::info!(target: "bex", operation = "client.connection.event",
                message = %format_args!("trace={} attempt={} report={} seq={} at_us={} phase={:?} group={} stream={} value={}",
                    performance.timeline.id, performance.attempt_id, performance.report,
                    event.sequence, event.at_us, event.phase, event.group, event.stream, event.value));
        }
    }
}

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
    tracing::subscriber::set_global_default(
        tracing_subscriber::registry()
            .with(log.with_filter(tracing_subscriber::filter::filter_fn(private_event)))
            .with(connection::network_layer()),
    )
    .map_err(io::Error::other)?;
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(target: "bex", operation = "panic", message = %info);
        previous(info);
    }));
    Ok(())
}

pub fn rpc_error(operation: &str, request_id: Option<u64>, raw: &RawValue) {
    match serde_json::from_str::<Value>(raw.get()) {
        Ok(value) => {
            let error = rpc_cause(&value);
            tracing::error!(target: "bex",
                operation,
                message = error.get("message").and_then(serde_json::Value::as_str)
                    .or_else(|| error.as_str()).unwrap_or("RPC error without a message"),
                request_id,
                error_code = %error.get("code").filter(|code| code.is_number() || code.is_string()).unwrap_or(&serde_json::Value::Null),
            );
        }
        Err(_) => tracing::error!(target: "bex", operation, request_id, "Malformed RPC error"),
    }
}

/// Select the provider cause from a Host failure without changing delivery evidence.
pub(crate) fn rpc_cause(error: &Value) -> &Value {
    error.get("providerError").unwrap_or(error)
}

/// Decode only error fields; ignore conversation bodies in turn notifications.
pub(crate) fn notification(message: &crate::peer::RpcMessage<'_>) {
    let Some(method @ ("error" | "turn/completed")) = message.method() else {
        return;
    };
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

// Only explicitly selected Bex diagnostic fields enter the private log.
// Dependency traces and arbitrary request/response fields are never persisted.
#[derive(Default)]
struct Fields {
    operation: String,
    message: String,
    request_id: Option<u64>,
    error_code: Option<Value>,
}

impl tracing::field::Visit for Fields {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "operation" => self.operation = value.into(),
            "message" => self.message = value.into(),
            "error_code" => self.error_code = Some(Value::String(value.into())),
            _ => (),
        }
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if field.name() == "request_id" {
            self.request_id = Some(value);
        } else if field.name() == "error_code" {
            self.error_code = Some(value.into());
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "operation" => self.operation = format!("{value:?}"),
            "message" => self.message = format!("{value:?}"),
            "error_code" => {
                self.error_code = serde_json::from_str::<Value>(&format!("{value:?}"))
                    .ok()
                    .filter(|code| code.is_number() || code.is_string())
            }
            _ => (),
        }
    }
}

fn private_event(metadata: &tracing::Metadata<'_>) -> bool {
    metadata.target() == "bex" && *metadata.level() <= tracing::Level::INFO
}

impl<S: tracing::Subscriber> Layer<S> for Log {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        if !private_event(event.metadata()) {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        let level = match *event.metadata().level() {
            tracing::Level::ERROR => "error",
            tracing::Level::WARN => "warn",
            _ => "info",
        };
        if let Err(error) = self.write(
            level,
            &fields.operation,
            &fields.message,
            fields.request_id,
            fields.error_code,
        ) {
            let _ = writeln!(
                io::stderr().lock(),
                "Bex error log could not be written: {error}"
            );
        }
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
        .map(rpc_cause)
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
    fn connection_measurements_are_written_to_the_private_host_log() {
        let directory = tempfile::tempdir().unwrap();
        let log = Log::open(
            directory.path(),
            Component::Host,
            FILE_BYTES,
            "test-version",
        )
        .unwrap();
        let path = log.path.clone();
        let subscriber = tracing_subscriber::registry().with(log);
        tracing::subscriber::with_default(subscriber, || {
            connection_performance(&ConnectionPerformance {
                total_ms: 1743,
                endpoint_ms: 53,
                transport_ms: 1083,
                verification_ms: 580,
                route: ConnectionRoute::Relay,
                platform: ClientPlatform::Ios,
                resolution_ms: 7,
                rtt_ms: 90,
                ..Default::default()
            });
        });
        let record: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(record["operation"], "client.connection");
        assert_eq!(
            record["message"],
            "platform=Ios total_ms=1743 endpoint_ms=53 transport_ms=1083 verification_ms=580 reused=false route=Relay resolution_ms=7 rtt_ms=90"
        );
        assert_eq!(record["component"], "host");
        assert!(record.get("requestId").is_none());
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
