use serde::{Deserialize, Serialize};
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
    pub client_revision: String,
    pub timeline: ConnectionTimeline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    ResumeConnection,
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
    NetworkReportStart,
    NetworkReportReady,
    RelayDialStart,
    RelayRegion,
    RelayDialEnded,
    RelayTcpStart,
    RelayTcpReady,
    RelayTlsStart,
    RelayTlsReady,
    RelayAuthStart,
    RelayAuthReady,
    RelayReady,
    RequestSlotWait,
    RequestOpened,
    RequestEncoded,
    RequestSent,
    ReplyAdopted,
    ReadPolled,
    ReadPending,
    ReadWake,
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
    RuntimePulse,
    AppScene,
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
