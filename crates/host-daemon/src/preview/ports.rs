//! Local development server discovery for Preview.
//!
//! Listener discovery is deliberately conservative: only loopback listeners
//! and explicitly configured loopback URLs are considered, and a listener is
//! published after a bounded HTML probe succeeds.  Probe results are keyed by
//! URL and listener identity so a process restart cannot reuse a stale result.
use agent_protocol::{
    operations::TerminalSummary,
    preview::{
        COMMON_DEV_PORTS, CONFIGURED_LOCAL_SERVER_URLS_MAX_ITEMS, DiscoveredLocalServer,
        PreviewTerminalOwner, PREVIEW_URL_MAX_LENGTH,
    },
};
use std::{
    collections::{HashMap, HashSet},
    process::Stdio,
    sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}},
    time::{Duration, Instant},
};
use tokio::{process::Command, sync::mpsc};

const CACHE_TTL: Duration = Duration::from_secs(15);
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PROBE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ProbeKey {
    url: String,
    pid: Option<u32>,
}
#[derive(Debug, Clone, Copy)]
struct ProbeResult {
    html: bool,
    expires_at: Instant,
}
#[derive(Debug, Clone)]
struct Listener {
    port: u16,
    process_name: Option<String>,
    pid: Option<u32>,
}

#[derive(Default)]
struct State {
    probes: HashMap<ProbeKey, ProbeResult>,
    terminal_owners: HashMap<u32, PreviewTerminalOwner>,
}

/// A reference-counted scanner.  Polling owners retain a lease; the count is
/// exposed for the Host's subscription lifecycle and tests.
pub struct PortScanner {
    state: Mutex<State>,
    retained: AtomicUsize,
}

pub struct RetainGuard {
    scanner: Arc<PortScanner>,
}
impl Drop for RetainGuard {
    fn drop(&mut self) {
        self.scanner.retained.fetch_sub(1, Ordering::AcqRel);
    }
}

pub struct Subscription {
    stop: tokio_util::sync::CancellationToken,
    _retain: RetainGuard,
}
impl Drop for Subscription {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

impl PortScanner {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::default()),
            retained: AtomicUsize::new(0),
        })
    }

    pub fn retain(self: &Arc<Self>) -> RetainGuard {
        self.retained.fetch_add(1, Ordering::AcqRel);
        RetainGuard {
            scanner: self.clone(),
        }
    }

    pub fn retain_count(&self) -> usize {
        self.retained.load(Ordering::Acquire)
    }

    /// Replaces the Host's current shell and subprocess ownership index. The
    /// listener PID is matched against both identities during the next scan.
    pub fn set_terminal_owners(&self, owners: Vec<(u32, PreviewTerminalOwner)>) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.terminal_owners = owners.into_iter().collect();
    }

    /// Starts a 3-second poll while the returned subscription is alive.
    pub fn subscribe(
        self: &Arc<Self>,
        configured_urls: Vec<String>,
        terminals: Vec<TerminalSummary>,
    ) -> (mpsc::Receiver<Vec<DiscoveredLocalServer>>, Subscription) {
        let (sender, receiver) = mpsc::channel(4);
        let stop = tokio_util::sync::CancellationToken::new();
        let lease = self.retain();
        let scanner = self.clone();
        let cancel = stop.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(3));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut previous: Option<Vec<DiscoveredLocalServer>> = None;
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = interval.tick() => {
                        let result = scanner.scan(&configured_urls, &terminals).await.unwrap_or_default();
                        if previous.as_ref() != Some(&result) {
                            previous = Some(result.clone());
                            if sender.send(result).await.is_err() { break; }
                        }
                    }
                }
            }
        });
        (receiver, Subscription { stop, _retain: lease })
    }

    /// Scans listeners, configured URLs and only publishes pages that answer
    /// as HTML over HTTP(S).
    pub async fn scan(
        &self,
        configured_urls: &[String],
        terminals: &[TerminalSummary],
    ) -> Result<Vec<DiscoveredLocalServer>, String> {
        let configured = normalize_configured_urls(configured_urls);
        let configured_ports: HashSet<u16> = configured.iter().map(|url| url_port(url)).collect();
        let mut listeners = self.listeners(&configured_ports).await?;
        for url in &configured {
            let port = url_port(url);
            if !listeners.iter().any(|listener| listener.port == port) {
                listeners.push(Listener {
                    port,
                    process_name: None,
                    pid: None,
                });
            }
        }
        listeners.sort_by_key(|listener| listener.port);
        listeners.dedup_by_key(|listener| listener.port);
        let mut owners = terminal_owners(terminals);
        owners.extend(
            self.state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .terminal_owners
                .clone(),
        );
        let mut found = Vec::new();
        for listener in listeners {
            let urls: Vec<String> = configured
                .iter()
                .filter(|url| url_port(url) == listener.port)
                .cloned()
                .collect();
            let urls = if urls.is_empty() {
                vec![
                    format!("http://localhost:{}/", listener.port),
                    format!("https://localhost:{}/", listener.port),
                ]
            } else {
                urls
            };
            let mut working = None;
            for url in urls {
                if self.probe(&url, listener.pid).await? {
                    working = Some(url);
                    break;
                }
            }
            let Some(url) = working else { continue };
            found.push(DiscoveredLocalServer {
                host: "localhost".into(),
                port: listener.port,
                url,
                process_name: listener.process_name,
                pid: listener.pid,
                terminal: listener.pid.and_then(|pid| owners.get(&pid).cloned()),
            });
        }
        Ok(found)
    }

    async fn listeners(&self, configured_ports: &HashSet<u16>) -> Result<Vec<Listener>, String> {
        #[cfg(unix)]
        {
            match self.lsof().await {
                Ok(listeners) if !listeners.is_empty() => return Ok(listeners),
                Ok(_) | Err(_) => {}
            }
        }
        #[cfg(windows)]
        {
            if let Ok(listeners) = self.windows_listeners().await {
                if !listeners.is_empty() {
                    return Ok(listeners);
                }
            }
        }
        let mut ports = COMMON_DEV_PORTS.to_vec();
        ports.extend(configured_ports.iter().copied());
        ports.sort_unstable();
        ports.dedup();
        let mut listeners = Vec::new();
        for port in ports {
            if tcp_listener_exists(port).await {
                listeners.push(Listener {
                    port,
                    process_name: None,
                    pid: None,
                });
            }
        }
        Ok(listeners)
    }

    #[cfg(unix)]
    async fn lsof(&self) -> Result<Vec<Listener>, String> {
        let output = tokio::time::timeout(
            COMMAND_TIMEOUT,
            Command::new("lsof")
                .args(["-nP", "-iTCP", "-sTCP:LISTEN", "-Fpcn"])
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output(),
        )
        .await
        .map_err(|_| "lsof timed out".to_owned())?
        .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err("lsof failed".into());
        }
        Ok(parse_lsof(&String::from_utf8_lossy(&output.stdout)))
    }

    #[cfg(windows)]
    async fn windows_listeners(&self) -> Result<Vec<Listener>, String> {
        let output = tokio::time::timeout(
            COMMAND_TIMEOUT,
            Command::new("powershell.exe")
                .args([
                    "-NoProfile",
                    "-Command",
                    "Get-NetTCPConnection -State Listen | ForEach-Object { \"$($_.LocalAddress)|$($_.LocalPort)|$($_.OwningProcess)\" }",
                ])
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output(),
        )
        .await
        .map_err(|_| "PowerShell timed out".to_owned())?
        .map_err(|error| error.to_string())?;
        Ok(parse_windows(&String::from_utf8_lossy(&output.stdout)))
    }

    async fn probe(&self, raw_url: &str, pid: Option<u32>) -> Result<bool, String> {
        let key = ProbeKey {
            url: canonical_probe_url(raw_url)?,
            pid,
        };
        if let Some(entry) = self
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .probes
            .get(&key)
            .copied()
            .filter(|entry| entry.expires_at > Instant::now())
        {
            return Ok(entry.html);
        }
        let html = probe_http(&key.url).await;
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .probes
            .insert(
                key,
                ProbeResult {
                    html,
                    expires_at: Instant::now() + CACHE_TTL,
                },
            );
        Ok(html)
    }
}

fn normalize_configured_urls(urls: &[String]) -> Vec<String> {
    urls.iter()
        .take(CONFIGURED_LOCAL_SERVER_URLS_MAX_ITEMS)
        .filter_map(|raw| canonical_configured_url(raw))
        .collect()
}

fn canonical_configured_url(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if raw.len() > PREVIEW_URL_MAX_LENGTH {
        return None;
    }
    let source = if raw.contains("://") {
        raw.to_owned()
    } else {
        format!("http://{raw}")
    };
    let mut url = url::Url::parse(&source).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || !is_loopback(url.host_str()?)
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    if url.host_str() == Some("0.0.0.0") {
        url.set_host(Some("localhost")).ok()?;
    }
    (url.as_str().len() <= PREVIEW_URL_MAX_LENGTH).then(|| url.to_string())
}

fn canonical_probe_url(raw: &str) -> Result<String, String> {
    let configured = canonical_configured_url(raw)
        .ok_or_else(|| "invalid local preview URL".to_owned())?;
    let mut url = url::Url::parse(&configured).map_err(|_| "invalid local preview URL")?;
    url.set_fragment(None);
    Ok(url.to_string())
}

fn url_port(url: &str) -> u16 {
    url::Url::parse(url)
        .ok()
        .and_then(|url| url.port_or_known_default())
        .unwrap_or(0)
}

fn is_loopback(host: &str) -> bool {
    matches!(
        host.trim_matches(['[', ']']).to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "0.0.0.0" | "::" | "::1"
    )
}

fn terminal_owners(terminals: &[TerminalSummary]) -> HashMap<u32, PreviewTerminalOwner> {
    terminals
        .iter()
        .filter_map(|terminal| {
            terminal.pid.map(|pid| {
                (
                    pid,
                    PreviewTerminalOwner {
                        thread_id: terminal.thread.clone(),
                        terminal_id: terminal.terminal_id.clone(),
                    },
                )
            })
        })
        .collect()
}

#[cfg(unix)]
fn parse_lsof(raw: &str) -> Vec<Listener> {
    let mut output = Vec::new();
    let mut pid = None;
    let mut process_name = None;
    for line in raw.lines().filter(|line| !line.is_empty()) {
        let (tag, value) = line.split_at(1);
        match tag {
            "p" => {
                pid = value.parse().ok();
                process_name = None;
            }
            "c" => process_name = (!value.trim().is_empty()).then(|| value.trim().to_owned()),
            "n" => {
                let name = value.split_whitespace().next().unwrap_or_default();
                let Some(port) = name.rsplit_once(':').and_then(|(_, port)| port.parse().ok())
                else {
                    continue;
                };
                let host = name.rsplit_once(':').map(|(host, _)| host).unwrap_or_default();
                if !(host.is_empty() || host == "*" || is_loopback(host.trim_matches(['[', ']']))) {
                    continue;
                }
                if !output.iter().any(|listener: &Listener| listener.port == port) {
                    output.push(Listener {
                        port,
                        process_name: process_name.clone(),
                        pid,
                    });
                }
            }
            _ => {}
        }
    }
    output.sort_by_key(|listener| listener.port);
    output
}

#[cfg(windows)]
fn parse_windows(raw: &str) -> Vec<Listener> {
    let mut output = Vec::new();
    for line in raw.lines() {
        let mut fields = line.split('|');
        let (Some(host), Some(port), Some(pid)) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        let (Ok(port), Ok(pid)) = (port.trim().parse(), pid.trim().parse()) else { continue };
        if is_loopback(host.trim()) && !output.iter().any(|listener: &Listener| listener.port == port) {
            output.push(Listener { port, process_name: None, pid: Some(pid) });
        }
    }
    output.sort_by_key(|listener| listener.port);
    output
}

async fn tcp_listener_exists(port: u16) -> bool {
    tokio::time::timeout(
        Duration::from_millis(250),
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .is_ok_and(|result| result.is_ok())
}

async fn probe_http(url: &str) -> bool {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    let client = match reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .danger_accept_invalid_certs(true)
        .timeout(PROBE_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(_) => return false,
    };
    let response = match client.get(url).send().await {
        Ok(response) => response,
        Err(_) => return false,
    };
    if response.status().is_redirection() {
        return response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| !value.trim().is_empty());
    }
    if !response.status().is_success() {
        return false;
    }
    if response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            let value = value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            matches!(value.as_str(), "text/html" | "application/xhtml+xml")
        })
    {
        return true;
    }
    let Ok(bytes) = response.bytes().await else { return false };
    if bytes.len() > MAX_PROBE_BYTES {
        return false;
    }
    let body = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
    body.contains("<html") || body.contains("<!doctype html")
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::parse_lsof;

    #[cfg(unix)]
    #[test]
    fn parses_lsof_listener_identity_and_ignores_remote_addresses() {
        let listeners = parse_lsof("p123\ncnode\nn*:5173\nn192.0.2.1:7000\np456\ncvite\nn127.0.0.1:4173\n");
        assert_eq!(listeners.len(), 2);
        assert_eq!(listeners[0].port, 4173);
        assert_eq!(listeners[1].pid, Some(123));
        assert_eq!(listeners[1].process_name.as_deref(), Some("node"));
    }

    #[test]
    fn configured_urls_are_canonicalized_to_loopback_and_preserve_fragments() {
        assert_eq!(
            super::canonical_configured_url("  http://0.0.0.0:5173/docs#ready ").as_deref(),
            Some("http://localhost:5173/docs#ready")
        );
        assert_eq!(
            super::canonical_configured_url("localhost:5173/docs").as_deref(),
            Some("http://localhost:5173/docs")
        );
        assert!(super::canonical_configured_url("http://192.0.2.1:5173").is_none());
    }

    #[test]
    fn subscriptions_hold_one_scanner_lease_until_dropped() {
        let scanner = super::PortScanner::new();
        assert_eq!(scanner.retain_count(), 0);
        let lease = scanner.retain();
        assert_eq!(scanner.retain_count(), 1);
        drop(lease);
        assert_eq!(scanner.retain_count(), 0);
    }
}
