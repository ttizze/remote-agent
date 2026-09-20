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
const WINDOW: Duration = Duration::from_secs(30);
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
}

pub(crate) struct Trace {
    pub id: u64,
    origin: Instant,
    state: Mutex<State>,
}

pub(crate) fn identifier() -> u64 {
    uuid::Uuid::new_v4().as_u128() as u64
}

impl Trace {
    pub fn new() -> Arc<Self> {
        let trace = Arc::new(Self {
            id: identifier(),
            origin: Instant::now(),
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

    pub fn activate(&self) {
        self.state.lock().unwrap().until = Instant::now() + WINDOW;
    }

    pub fn record(&self, phase: ConnectionPhase, group: u64, stream: u64, value: u64) {
        let mut state = self.state.lock().unwrap();
        if Instant::now() > state.until
            && !matches!(
                phase,
                ConnectionPhase::UiConnectFailed
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
    }

    pub fn snapshot(&self) -> ConnectionTimeline {
        let state = self.state.lock().unwrap();
        ConnectionTimeline {
            id: self.id,
            dropped: state.sequence.saturating_sub(state.events.len() as u64),
            events: state.events.iter().cloned().collect(),
        }
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
    value: u64,
    region: u64,
}

impl tracing::field::Visit for Fields {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        match field.name() {
            "trace_id" => self.trace_id = value,
            "value" => self.value = value,
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
            trace.record(phase, group, span.id().into_u64(), fields.value);
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
        assert!(
            snapshot
                .events
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence && pair[0].at_us <= pair[1].at_us)
        );
    }
}
