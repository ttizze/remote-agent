//! Bounded, monotonic connection timelines. No dependency messages or identities.
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, LazyLock, Mutex, Once, Weak},
    time::{Duration, Instant},
};
use tracing::Subscriber;
use tracing_subscriber::{Layer, layer::SubscriberExt, registry::LookupSpan};

const CAPACITY: usize = 768;
const NETWORK_CAPACITY: usize = 8192;
pub(crate) const MAX_EVENTS: usize = CAPACITY + NETWORK_CAPACITY;
const WINDOW: Duration = Duration::from_secs(30);
mod quic;
static TRACES: LazyLock<Mutex<HashMap<u64, Weak<Trace>>>> = LazyLock::new(Default::default);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ConnectionPhase {
    AppPreparation,
    SnapshotRead,
    StoreRestored,
    ClientBuild,
    IdentityRead,
    UiConnectStart,
    UiConnectReady,
    UiConnectFailed,
    UiConnectCancelled,
    ListPublished,
    ListViewUpdated,
    ResumeStart,
    ResumeReady,
    ResumeFailed,
    ResumeCancelled,
    ResolveFailed,
    QuicFailed,
    NetworkCapture,
    EndpointStart,
    EndpointReady,
    HostDnsStart,
    HostDnsReady,
    HostDnsEnded,
    ResolveStart,
    ResolveReady,
    QuicStart,
    QuicReady,
    EventsOpened,
    AttachStart,
    AttachReady,
    RelayDialStart,
    RelayRegion,
    RelayDialEnded,
    RelayDnsStart,
    RelayDnsAddress,
    RelayDnsFailed,
    RelayDnsFinished,
    RelayTcpStart,
    RelayTcpReady,
    RelayTcpFailed,
    RelayTlsStart,
    RelayTlsReady,
    RelayWebsocketStart,
    RelayWebsocketReady,
    RelayAuthStart,
    RelayAuthReady,
    RelayReady,
    RequestOpened,
    RequestSent,
    ReplyAdopted,
    ResponseFirstRead,
    ResponseReceived,
    ResponseDecoded,
    RequestFailed,
    PathOpened,
    PathClosed,
    PathSelected,
    PathEventsDropped,
    PathDirect,
    PathRelay,
    PathUnknown,
    RttMicros,
    LostPackets,
    LostBytes,
    CryptoFramesSent,
    CryptoFramesReceived,
    SentPackets,
    ReceivedPackets,
    QuicTraceLinked,
    QuicPacketSent,
    QuicPacketReceived,
    QuicPacketLost,
    QuicFrameSent,
    QuicFrameReceived,
    QuicTimer,
    QuicMetric,
    QuicTraceMalformed,
    RelayDatagramSent,
    RelayDatagramReceived,
    RelayPingSent,
    RelayPongReceived,
    RelaySendReady,
    RelaySendPending,
    RelayFlushReady,
    RelayFlushPending,
    RelayReadPending,
    RelayReadError,
    RelayWriteError,
    RelayTcpRead,
    RelayTcpWrite,
    RelayTcpReadPending,
    RelayTcpWritePending,
    RelayTcpReadWake,
    RelayTcpWriteWake,
    RelayTcpMetric,
    RelayTcpInfoUnavailable,
    RuntimePulse,
    AppScene,
    ReadPolled,
    ReadPending,
    ReadWake,
}

/// Numeric metadata only. Meaning is fixed by phase; never contains packet payloads.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct NetworkDetail {
    pub kind: u64,
    pub space: u64,
    pub packet: u64,
    pub packet_valid: bool,
    pub offset: u64,
    pub length: u64,
    pub path: u64,
    pub source_at_us: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionEvent {
    pub sequence: u64,
    pub at_us: u64,
    pub phase: ConnectionPhase,
    /// Connection or relay dial identifier, never a peer identity.
    pub group: u64,
    pub stream: u64,
    pub value: u64,
    pub detail: Option<NetworkDetail>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConnectionTimeline {
    pub id: u64,
    pub dropped: u64,
    pub events: Vec<ConnectionEvent>,
}

struct State {
    until: Instant,
    sequence: u64,
    events: VecDeque<ConnectionEvent>,
    network: VecDeque<ConnectionEvent>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Off,
    Stages,
    Packets,
}

pub(crate) struct Trace {
    level: Level,
    pub id: u64,
    origin: Instant,
    state: Mutex<State>,
    continuous: bool,
}

pub(crate) fn identifier() -> u64 {
    uuid::Uuid::new_v4().as_u128() as u64
}

impl Trace {
    #[cfg(any(feature = "bindings", test))]
    pub fn new() -> Arc<Self> {
        Self::create(false)
    }

    pub fn continuous() -> Arc<Self> {
        Self::create(true)
    }

    fn create(continuous: bool) -> Arc<Self> {
        let level = match std::env::var("BEX_CONNECTION_DIAGNOSTICS").as_deref() {
            Ok("off") => Level::Off,
            Ok("stages") => Level::Stages,
            _ => Level::Packets,
        };
        Self::with_level(continuous, level)
    }

    fn with_level(continuous: bool, level: Level) -> Arc<Self> {
        let trace = Arc::new(Self {
            level,
            id: identifier(),
            origin: Instant::now(),
            state: Mutex::new(State {
                until: Instant::now() + WINDOW,
                sequence: 0,
                events: VecDeque::new(),
                network: VecDeque::new(),
            }),
            continuous,
        });
        let mut traces = TRACES.lock().unwrap();
        traces.retain(|_, value| value.strong_count() > 0);
        traces.insert(trace.id, Arc::downgrade(&trace));
        trace
    }

    pub fn activate(&self) {
        if !self.enabled() {
            return;
        }
        self.state.lock().unwrap().until = Instant::now() + WINDOW;
    }

    pub fn record(&self, phase: ConnectionPhase, group: u64, stream: u64, value: u64) {
        self.record_detail(phase, group, stream, value, None);
    }

    pub fn enabled(&self) -> bool {
        self.level != Level::Off
    }
    pub fn packets(&self) -> bool {
        self.level == Level::Packets
    }
    pub fn active(&self) -> bool {
        self.enabled() && (self.continuous || Instant::now() <= self.state.lock().unwrap().until)
    }
    pub fn elapsed_at(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.origin).as_micros() as u64
    }

    pub fn record_detail(
        &self,
        phase: ConnectionPhase,
        group: u64,
        stream: u64,
        value: u64,
        detail: Option<NetworkDetail>,
    ) {
        let network = detail.is_some() || phase == ConnectionPhase::RuntimePulse;
        if !self.enabled() || (!self.packets() && network) {
            return;
        }
        let mut state = self.state.lock().unwrap();
        if !self.continuous
            && Instant::now() > state.until
            && !matches!(
                phase,
                ConnectionPhase::UiConnectFailed
                    | ConnectionPhase::UiConnectCancelled
                    | ConnectionPhase::ResumeFailed
                    | ConnectionPhase::ResumeCancelled
                    | ConnectionPhase::RequestFailed
                    | ConnectionPhase::ResolveFailed
                    | ConnectionPhase::QuicFailed
                    | ConnectionPhase::AppScene
            )
        {
            return;
        }
        state.sequence += 1;
        let sequence = state.sequence;
        let queue = if network {
            &mut state.network
        } else {
            &mut state.events
        };
        if queue.len() == if network { NETWORK_CAPACITY } else { CAPACITY } {
            queue.pop_front();
        }
        queue.push_back(ConnectionEvent {
            sequence,
            at_us: self.origin.elapsed().as_micros() as u64,
            phase,
            group,
            stream,
            value,
            detail,
        });
    }

    pub fn snapshot(&self) -> ConnectionTimeline {
        let (mut events, sequence) = {
            let state = self.state.lock().unwrap();
            (
                state
                    .events
                    .iter()
                    .chain(&state.network)
                    .cloned()
                    .collect::<Vec<_>>(),
                state.sequence,
            )
        };
        events.sort_unstable_by_key(|event| event.sequence);
        ConnectionTimeline {
            id: self.id,
            dropped: sequence.saturating_sub(events.len() as u64),
            events,
        }
    }

    pub fn log_host_snapshot(&self) {
        if !self.enabled() {
            return;
        }
        let mut timeline = self.snapshot();
        let oldest = self.origin.elapsed().as_micros().saturating_sub(45_000_000) as u64;
        timeline
            .events
            .retain(|event| event.detail.is_none() || event.at_us >= oldest);
        super::host_connection_timeline(&timeline);
    }

    /// Weak ownership: sampling does not keep the endpoint or Store alive.
    pub fn sample_runtime(self: &Arc<Self>) {
        if !self.packets() {
            return;
        }
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut previous = Instant::now();
            loop {
                tokio::time::sleep(Duration::from_millis(250)).await;
                let Some(trace) = weak.upgrade() else { break };
                trace.record(
                    ConnectionPhase::RuntimePulse,
                    0,
                    0,
                    previous.elapsed().as_micros() as u64,
                );
                previous = Instant::now();
            }
        });
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
    NetworkLayer.with_filter(
        tracing_subscriber::filter::dynamic_filter_fn(|metadata, _| capture_metadata(metadata))
            .with_callsite_filter(|metadata| {
                if metadata.target() == "bex.net.packet" {
                    tracing::subscriber::Interest::sometimes()
                } else if capture_metadata(metadata) {
                    tracing::subscriber::Interest::always()
                } else {
                    tracing::subscriber::Interest::never()
                }
            }),
    )
}

fn capture_metadata(metadata: &tracing::Metadata<'_>) -> bool {
    metadata.target().starts_with("bex.net")
        || metadata.target() == "iroh::address_lookup::dns"
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
    value: u64,
    region: u64,
    network: bool,
    detail: NetworkDetail,
}

impl tracing::field::Visit for Fields {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        match field.name() {
            "trace_id" => self.trace_id = value,
            "value" => self.value = value,
            "length" => self.detail.length = value,
            "metric" => self.detail.kind = value,
            "offset" => self.detail.offset = value,
            _ => (),
        }
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() != "phase" {
            return;
        }
        use ConnectionPhase::*;
        self.phase = match value {
            "relay_dns_start" => Some(RelayDnsStart),
            "relay_dns_address" => Some(RelayDnsAddress),
            "relay_dns_failed" => Some(RelayDnsFailed),
            "relay_dns_finished" => Some(RelayDnsFinished),
            "relay_tcp_start" => Some(RelayTcpStart),
            "relay_tcp_ready" => Some(RelayTcpReady),
            "relay_tcp_failed" => Some(RelayTcpFailed),
            "relay_tls_start" => Some(RelayTlsStart),
            "relay_tls_ready" => Some(RelayTlsReady),
            "relay_websocket_start" => Some(RelayWebsocketStart),
            "relay_websocket_ready" => Some(RelayWebsocketReady),
            "relay_auth_start" => Some(RelayAuthStart),
            "relay_auth_ready" => Some(RelayAuthReady),
            "relay_ready" => Some(RelayReady),
            _ => None,
        };
        if self.phase.is_none() {
            self.phase = match value {
                "relay_datagram_sent" => Some(RelayDatagramSent),
                "relay_datagram_received" => Some(RelayDatagramReceived),
                "relay_ping_sent" => Some(RelayPingSent),
                "relay_pong_received" => Some(RelayPongReceived),
                "relay_send_ready" => Some(RelaySendReady),
                "relay_send_pending" => Some(RelaySendPending),
                "relay_flush_ready" => Some(RelayFlushReady),
                "relay_flush_pending" => Some(RelayFlushPending),
                "relay_read_pending" => Some(RelayReadPending),
                "relay_read_error" => Some(RelayReadError),
                "relay_write_error" => Some(RelayWriteError),
                "relay_tcp_read" => Some(RelayTcpRead),
                "relay_tcp_write" => Some(RelayTcpWrite),
                "relay_tcp_read_pending" => Some(RelayTcpReadPending),
                "relay_tcp_write_pending" => Some(RelayTcpWritePending),
                "relay_tcp_read_wake" => Some(RelayTcpReadWake),
                "relay_tcp_write_wake" => Some(RelayTcpWriteWake),
                "relay_tcp_metric" => Some(RelayTcpMetric),
                "relay_tcp_info_unavailable" => Some(RelayTcpInfoUnavailable),
                _ => None,
            };
            self.network = self.phase.is_some();
        }
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
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
    fn enabled(
        &self,
        metadata: &tracing::Metadata<'_>,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        if metadata.target() != "bex.net.packet" {
            return true;
        }
        // A per-layer filter can return false positives from event_enabled!.
        // This exclusive diagnostic target needs a global gate so disabled
        // capture also avoids hashes, socket samplers and forwarding wakers.
        ctx.lookup_current()
            .and_then(|span| span.extensions().get::<Context>().cloned())
            .and_then(|context| context.trace.upgrade())
            .is_some_and(|trace| trace.packets() && trace.active())
    }

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
        let Some(span) = ctx.event_span(event) else {
            return;
        };
        let ext = span.extensions();
        let Some(context) = ext.get::<Context>() else {
            return;
        };
        if let Some(trace) = context.trace.upgrade() {
            // TCP attempt span distinguishes parallel IPv4/IPv6 dials.
            let group = if phase == ConnectionPhase::HostDnsReady {
                span.id().into_u64()
            } else {
                context.dial
            };
            trace.record_detail(
                phase,
                group,
                span.id().into_u64(),
                fields.value,
                fields.network.then_some(fields.detail),
            );
        }
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
    fn disabled_capture_skips_packet_work_and_records_nothing() {
        use noq_proto::QlogFactory;
        for level in [Level::Off, Level::Stages, Level::Packets] {
            let trace = Trace::with_level(false, level);
            tracing::subscriber::with_default(
                tracing_subscriber::registry().with(network_layer()),
                || {
                    let span =
                        tracing::info_span!(target: "bex.net", "network", trace_id = trace.id);
                    let _entered = span.enter();
                    assert_eq!(
                        tracing::event_enabled!(target: "bex.net.packet", tracing::Level::TRACE),
                        level == Level::Packets
                    );
                    let config = quic::Factory {
                        trace: Arc::downgrade(&trace),
                        connection: 1,
                    }
                    .for_connection(
                        noq_proto::Side::Client,
                        "127.0.0.1:1".parse().unwrap(),
                        noq_proto::ConnectionId::new(&[1; 8]),
                        Instant::now(),
                    );
                    assert_eq!(config.is_some(), level == Level::Packets);
                    trace.record(ConnectionPhase::ResumeReady, 1, 0, 1);
                    trace.record_detail(
                        ConnectionPhase::ReadPending,
                        1,
                        0,
                        1,
                        Some(Default::default()),
                    );
                },
            );
            let snapshot = trace.snapshot();
            assert_eq!(snapshot.events.is_empty(), level == Level::Off);
            assert_eq!(
                snapshot.events.iter().any(|e| e.detail.is_some()),
                level == Level::Packets
            );
        }
    }

    #[tokio::test]
    #[ignore = "connects to the public iroh relay; run explicitly for release verification"]
    async fn real_relay_records_dns_tcp_tls_websocket_and_authentication() {
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
                    .any(|event| event.phase == ConnectionPhase::RelayPongReceived)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let snapshot = trace.snapshot();
            use ConnectionPhase::*;
            for phase in [
                NetworkCapture,
                RelayDialStart,
                RelayDnsStart,
                RelayDnsAddress,
                RelayTcpStart,
                RelayTcpReady,
                RelayTlsStart,
                RelayTlsReady,
                RelayWebsocketStart,
                RelayWebsocketReady,
                RelayAuthStart,
                RelayAuthReady,
                RelayReady,
                RelayPingSent,
                RelayPongReceived,
                RelayTcpRead,
                RelayTcpWrite,
            ] {
                assert!(
                    snapshot.events.iter().any(|event| event.phase == phase),
                    "missing {phase:?}: {snapshot:?}"
                );
            }
            #[cfg(target_vendor = "apple")]
            assert!(
                snapshot
                    .events
                    .iter()
                    .any(|event| event.phase == RelayTcpMetric)
            );
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
                tracing::trace!(target: "bex.net.stage", phase = "relay_tls_ready", token = "private-secret");
                tracing::trace!(target: "bex.net.stage", phase = "private-secret");
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
        for _ in 0..NETWORK_CAPACITY + 2 {
            trace.record_detail(
                ConnectionPhase::QuicPacketSent,
                1,
                0,
                0,
                Some(Default::default()),
            );
        }
        let snapshot = trace.snapshot();
        assert_eq!(snapshot.events.len(), MAX_EVENTS);
        assert_eq!(
            snapshot
                .events
                .iter()
                .filter(|event| event.phase == ConnectionPhase::ResumeStart)
                .count(),
            CAPACITY,
            "packet traffic must not evict connection milestones"
        );
        assert_eq!(snapshot.dropped, 7);
        assert!(
            snapshot
                .events
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence && pair[0].at_us <= pair[1].at_us)
        );
    }
}
