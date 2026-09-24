//! Bounded, monotonic connection timelines. No dependency messages or identities.
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, LazyLock, Mutex, Once, OnceLock, Weak},
    time::{Duration, Instant},
};
use tracing::Subscriber;
use tracing_subscriber::{Layer, layer::SubscriberExt, registry::LookupSpan};

pub(crate) const CAPACITY: usize = 768;
pub(crate) const WINDOW: Duration = Duration::from_secs(30);
static TRACES: LazyLock<Mutex<HashMap<u64, Weak<Trace>>>> = LazyLock::new(Default::default);

pub use agent_protocol::diagnostics::{ConnectionEvent, ConnectionPhase, ConnectionTimeline};
struct State {
    until: Instant,
    sequence: u64,
    events: VecDeque<ConnectionEvent>,
}

pub struct Trace {
    pub id: u64,
    origin: Instant,
    started_at_ms: u64,
    journal: OnceLock<super::journal::Journal>,
    enabled: bool,
    sampler: Mutex<Option<tokio_util::task::AbortOnDropHandle<()>>>,
    state: Mutex<State>,
}

pub fn identifier() -> u64 {
    uuid::Uuid::new_v4().as_u128() as u64
}

impl Trace {
    pub fn new() -> Arc<Self> {
        Self::with_enabled(std::env::var("BEX_CONNECTION_DIAGNOSTICS").as_deref() != Ok("off"))
    }
    fn with_enabled(enabled: bool) -> Arc<Self> {
        let trace = Arc::new(Self {
            id: identifier(),
            origin: Instant::now(),
            started_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            journal: OnceLock::new(),
            enabled,
            sampler: Mutex::new(None),
            state: Mutex::new(State {
                until: Instant::now() + WINDOW,
                sequence: 0,
                events: VecDeque::new(),
            }),
        });
        let mut traces = TRACES.lock().unwrap();
        traces.retain(|_, value| value.strong_count() > 0);
        traces.insert(trace.id, Arc::downgrade(&trace));
        trace
    }

    pub fn persist(&self, directory: std::path::PathBuf) -> std::io::Result<()> {
        if self.enabled && self.journal.get().is_none() {
            let journal = super::journal::Journal::open(directory, self.id)?;
            let _ = self.journal.set(journal);
        }
        Ok(())
    }

    pub async fn pending_reports(&self) -> Vec<super::ConnectionPerformance> {
        match self.journal.get() {
            Some(journal) => journal.read().await,
            None => Vec::new(),
        }
    }

    pub async fn acknowledge(&self, report: &super::ConnectionPerformance) {
        if let Some(journal) = self.journal.get() {
            journal.acknowledge(report).await;
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn active(&self) -> bool {
        self.enabled && Instant::now() <= self.state.lock().unwrap().until
    }

    pub fn elapsed_at(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.origin).as_micros() as u64
    }

    pub fn activate(self: &Arc<Self>) {
        if !self.enabled {
            return;
        }
        self.state.lock().unwrap().until = Instant::now() + WINDOW;
        // Swift's synchronous lifecycle callbacks have no Tokio context.
        // The async Core resume starts the sampler; UI markers still use this clock.
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let mut sampler = self.sampler.lock().unwrap();
        let weak = Arc::downgrade(self);
        let mut previous = Instant::now();
        *sampler = Some(tokio_util::task::AbortOnDropHandle::new(runtime.spawn(
            async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    let Some(trace) = weak.upgrade() else {
                        break;
                    };
                    let now = Instant::now();
                    trace.record(
                        ConnectionPhase::RuntimePulse,
                        0,
                        0,
                        now.duration_since(previous).as_micros() as u64,
                    );
                    if !trace.active() {
                        break;
                    }
                    previous = now;
                }
            },
        )));
    }

    pub fn record(&self, phase: ConnectionPhase, group: u64, stream: u64, value: u64) {
        if !self.enabled {
            return;
        }
        let mut state = self.state.lock().unwrap();
        if Instant::now() > state.until
            && !matches!(
                phase,
                ConnectionPhase::RuntimePulse
                    | ConnectionPhase::AppScene
                    | ConnectionPhase::UiConnectFailed
                    | ConnectionPhase::UiConnectCancelled
                    | ConnectionPhase::ResumeFailed
                    | ConnectionPhase::ResumeCancelled
                    | ConnectionPhase::RequestFailed
                    | ConnectionPhase::ResolveFailed
                    | ConnectionPhase::QuicFailed
            )
        {
            return;
        }
        state.sequence += 1;
        let sequence = state.sequence;
        if state.events.len() == CAPACITY {
            state.events.pop_front();
        }
        state.events.push_back(ConnectionEvent {
            sequence,
            at_us: self.origin.elapsed().as_micros() as u64,
            phase,
            group,
            stream,
            value,
        });
        drop(state);
        if matches!(
            phase,
            ConnectionPhase::RuntimePulse
                | ConnectionPhase::ResumeStart
                | ConnectionPhase::ResumeReady
                | ConnectionPhase::ResumeConnection
                | ConnectionPhase::UiConnectReady
                | ConnectionPhase::ResumeFailed
                | ConnectionPhase::ResumeCancelled
                | ConnectionPhase::UiConnectFailed
                | ConnectionPhase::UiConnectCancelled
                | ConnectionPhase::AppScene
        ) {
            self.checkpoint();
        }
    }

    fn checkpoint(&self) {
        let Some(journal) = self.journal.get() else {
            return;
        };
        let timeline = self.snapshot();
        let attempt_id = timeline
            .events
            .iter()
            .rev()
            .find(|event| event.phase == ConnectionPhase::ResumeStart)
            .map_or(0, |event| event.group);
        let connection_id = timeline
            .events
            .iter()
            .rev()
            .find(|event| {
                event.phase == ConnectionPhase::ResumeConnection && event.group == attempt_id
            })
            .map_or(0, |event| event.stream);
        journal.checkpoint(super::ConnectionPerformance {
            recovered: true,
            attempt_id,
            connection_id,
            platform: super::ClientPlatform::current(),
            client_revision: option_env!("BEX_BUILD_REVISION")
                .unwrap_or("development")
                .into(),
            timeline,
            ..Default::default()
        });
    }

    pub fn snapshot(&self) -> ConnectionTimeline {
        let state = self.state.lock().unwrap();
        ConnectionTimeline {
            started_at_ms: self.started_at_ms,
            id: self.id,
            dropped: state.sequence.saturating_sub(state.events.len() as u64),
            events: state.events.iter().cloned().collect(),
        }
    }
}

impl Drop for Trace {
    fn drop(&mut self) {
        self.checkpoint();
    }
}

/// Native mobile has no file logger. Desktop/Host install this with their logger.
pub(crate) fn initialize_mobile() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = tracing::subscriber::set_global_default(
            tracing_subscriber::registry().with(network_layer()),
        );
    });
}

pub(super) fn network_layer<S>() -> impl Layer<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    NetworkLayer.with_filter(tracing_subscriber::filter::filter_fn(|metadata| {
        matches!(
            metadata.target(),
            "bex.net"
                | "iroh::net_report"
                | "iroh_relay::client"
                | "iroh_relay::client::tls"
                | "iroh_relay::client::conn"
        ) || metadata.target() == "iroh::address_lookup::dns"
            || (metadata.is_span()
                && metadata.target().starts_with("iroh")
                && matches!(
                    metadata.name(),
                    "endpoint"
                        | "actor"
                        | "relay-actor"
                        | "active-relay"
                        | "dialing"
                        | "connected"
                        | "connect"
                        | "RemoteStateActor"
                        | "DnsAddressLookup"
                ))
    }))
}

struct NetworkLayer;

#[derive(Clone)]
struct Context {
    trace: Weak<Trace>,
    dial: u64,
    region: u64,
}

#[derive(Default)]
struct Fields {
    trace_id: u64,
    phase: Option<ConnectionPhase>,
    region: u64,
}

impl tracing::field::Visit for Fields {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if field.name() == "trace_id" {
            self.trace_id = value;
        }
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() != "message" {
            return;
        }
        use ConnectionPhase::*;
        self.phase = match value {
            "net_report starting" => Some(NetworkReportStart),
            "net_report generated" => Some(NetworkReportReady),
            "connecting TCP stream" => Some(RelayTcpStart),
            "TCP stream connected" => Some(RelayTcpReady),
            "Starting TLS handshake" => Some(RelayTlsStart),
            "tls_connector connect success" => Some(RelayTlsReady),
            "server_handshake: started" => Some(RelayAuthStart),
            "server_handshake: done" => Some(RelayAuthReady),
            "connect done" => Some(RelayReady),
            _ => None,
        };
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.record_str(field, &format!("{value:?}"));
        }
        if field.name() == "url"
            && let Ok(url) = url::Url::parse(&format!("{value:?}"))
            && let Some(host) = url.host_str().map(|host| host.trim_end_matches('.'))
            && host.ends_with(".relay.n0.iroh.link")
        {
            self.region = match host.split('-').next().unwrap_or("") {
                "aps1" => 1,
                "usw1" => 2,
                "use1" => 3,
                "euw1" => 4,
                _ => 0,
            };
        }
    }
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for NetworkLayer {
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let Some(span) = ctx.span(id) else { return };
        let mut inherited = span
            .parent()
            .and_then(|p| p.extensions().get::<Context>().cloned());
        if attrs.metadata().target() == "bex.net" {
            let mut fields = Fields::default();
            attrs.record(&mut fields);
            inherited = TRACES
                .lock()
                .unwrap()
                .get(&fields.trace_id)
                .map(|trace| Context {
                    trace: trace.clone(),
                    dial: 0,
                    region: 0,
                });
        }
        if let Some(mut inherited) = inherited {
            if attrs.metadata().name() == "active-relay" {
                let mut fields = Fields::default();
                attrs.record(&mut fields);
                inherited.region = fields.region;
            }
            if attrs.metadata().target() == "bex.net"
                && let Some(trace) = inherited.trace.upgrade()
            {
                trace.record(ConnectionPhase::NetworkCapture, 0, 0, 1);
            }
            if attrs.metadata().name() == "dialing" {
                inherited.dial = id.into_u64();
                if let Some(trace) = inherited.trace.upgrade() {
                    trace.record(ConnectionPhase::RelayDialStart, inherited.dial, 0, 0);
                    trace.record(
                        ConnectionPhase::RelayRegion,
                        inherited.dial,
                        0,
                        inherited.region,
                    );
                }
            }
            if attrs.metadata().name() == "DnsAddressLookup"
                && let Some(trace) = inherited.trace.upgrade()
            {
                trace.record(ConnectionPhase::HostDnsStart, id.into_u64(), 0, 0);
            }
            span.extensions_mut().insert(inherited);
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>, ctx: tracing_subscriber::layer::Context<'_, S>) {
        let Some(span) = ctx.event_span(event) else {
            return;
        };
        let ext = span.extensions();
        let Some(context) = ext.get::<Context>() else {
            return;
        };
        let Some(trace) = context.trace.upgrade().filter(|trace| trace.active()) else {
            return;
        };
        let mut fields = Fields::default();
        if event.metadata().target() == "iroh::address_lookup::dns" {
            // Upstream's success event has an `info` field. Inspect only its name;
            // never visit the endpoint information or arbitrary dependency messages.
            if event.metadata().fields().field("info").is_some() {
                fields.phase = Some(ConnectionPhase::HostDnsReady);
            }
        } else {
            event.record(&mut fields);
        }
        let Some(phase) = fields.phase else { return };
        // TCP attempt span distinguishes parallel IPv4/IPv6 dials.
        let group = if phase == ConnectionPhase::HostDnsReady {
            span.id().into_u64()
        } else {
            context.dial
        };
        trace.record(phase, group, span.id().into_u64(), 0);
    }

    fn on_close(&self, id: tracing::span::Id, ctx: tracing_subscriber::layer::Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let phase = match span.name() {
            "dialing" => ConnectionPhase::RelayDialEnded,
            "DnsAddressLookup" => ConnectionPhase::HostDnsEnded,
            _ => return,
        };
        if let Some(context) = span.extensions().get::<Context>()
            && let Some(trace) = context.trace.upgrade()
        {
            trace.record(phase, id.into_u64(), 0, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_lifecycle_callbacks_can_activate_without_a_runtime() {
        let trace = Trace::with_enabled(true);
        trace.activate();
        trace.record(ConnectionPhase::UiConnectStart, 0, 0, 0);
        assert!(trace.active());
        assert_eq!(trace.snapshot().events.len(), 1);
        assert!(trace.sampler.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn disabled_capture_has_no_events_or_sampler_and_runtime_stalls_are_observable() {
        let off = Trace::with_enabled(false);
        off.activate();
        off.record(ConnectionPhase::ResumeStart, 0, 0, 0);
        assert!(off.snapshot().events.is_empty());
        assert!(off.sampler.lock().unwrap().is_none());
        let trace = Trace::with_enabled(true);
        trace.activate();
        tokio::task::yield_now().await;
        std::thread::sleep(Duration::from_millis(350));
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(trace.snapshot().events.iter().any(|event| event.phase == ConnectionPhase::RuntimePulse && event.value >= 350_000));
    }

    #[tokio::test]
    #[ignore = "connects to the public iroh relay; run explicitly for release verification"]
    async fn real_relay_records_upstream_connection_boundaries() {
        use crate::transport::{Endpoint, Identity, Relays};
        tokio::time::timeout(Duration::from_secs(45), async {
            let trace = Trace::new();
            let endpoint =
                Endpoint::bind_recording(Identity::generate(), Relays::Default, trace.clone())
                    .await
                    .unwrap();
            loop {
                if trace
                    .snapshot()
                    .events
                    .iter()
                    .any(|event| event.phase == ConnectionPhase::RelayReady)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let snapshot = trace.snapshot();
            use ConnectionPhase::*;
            for phase in [
                NetworkCapture,
                NetworkReportStart,
                NetworkReportReady,
                RelayDialStart,
                RelayTcpStart,
                RelayTcpReady,
                RelayTlsStart,
                RelayTlsReady,
                RelayAuthStart,
                RelayAuthReady,
                RelayReady,
            ] {
                assert!(
                    snapshot.events.iter().any(|event| event.phase == phase),
                    "missing {phase:?}: {snapshot:?}"
                );
            }
            println!("{}", serde_json::to_string(&snapshot).unwrap());
            endpoint.close().await;
        })
        .await
        .unwrap();
    }

    #[test]
    fn timeline_is_bounded_and_ignores_private_dependency_fields() {
        let trace = Trace::new();
        tracing::subscriber::with_default(
            tracing_subscriber::registry().with(network_layer()),
            || {
                let root = tracing::info_span!(target: "bex.net", "network", trace_id = trace.id);
                let _root = root.enter();
                let dial =
                    tracing::info_span!(target: "iroh::relay", "dialing", url = "private-address");
                let _dial = dial.enter();
                tracing::trace!(target: "iroh_relay::client::tls", token = "private-secret", "tls_connector connect success");
                tracing::trace!(target: "iroh_relay::client::tls", "private-secret");
            },
        );
        let snapshot = trace.snapshot();
        assert_eq!(snapshot.events.len(), 5);
        assert_eq!(snapshot.events[3].phase, ConnectionPhase::RelayTlsReady);
        assert_ne!(snapshot.events[3].group, 0);
        assert!(
            !serde_json::to_string(&snapshot)
                .unwrap()
                .contains("private")
        );
        for _ in 0..CAPACITY {
            trace.record(ConnectionPhase::ResumeStart, 1, 0, 0);
        }
        let snapshot = trace.snapshot();
        assert_eq!(snapshot.events.len(), CAPACITY);
        assert_eq!(snapshot.dropped, 5);
        assert!(
            snapshot
                .events
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence && pair[0].at_us <= pair[1].at_us)
        );
    }
}
