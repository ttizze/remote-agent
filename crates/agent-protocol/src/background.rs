//! Background activity and diagnostics RPC values.
//!
//! The value types are shared with the pure domain rules so the Host and
//! clients cannot silently reimplement lease or resource semantics.
pub use agent_domain::{
    BackgroundActivityPolicy, BackgroundActivityProfile, BackgroundAppState,
    BackgroundBatteryState, BackgroundBooleanState, BackgroundClientKind, BackgroundPolicySnapshot,
    BackgroundScope, ClientActivityLease, ClientActivityReport, HostPowerSnapshot, HostPowerSource,
    HostPowerThermalState,
    ProcessDiagnosticsEntry, ProcessDiagnosticsError, ProcessDiagnosticsResult,
    ProcessResourceEntry, ProcessResourceHistoryBucket, ProcessResourceHistoryError,
    ProcessResourceHistoryResult,
    ResourceAggregate, ResourceAttributionEntry,
    ResourceAttributionSnapshot, ResourceGroups, ResourceHealth, ResourceHistoryBucket,
    ResourceProcess, ResourceProcessCategory, ResourceProcessIdentity, ResourceProcessSample,
    ResourceProcessSummary, ResourceSourceHealth, ResourceSourceStatus,
    ResourceTelemetryHistory, ResourceTelemetryIoSemantics, ResourceTelemetrySnapshot,
    TraceDiagnosticsError, TraceDiagnosticsErrorKind, TraceDiagnosticsResult, TraceFailureSummary,
    TraceLogEvent, TraceRecentFailure, TraceSpanOccurrence, TraceSpanSummary,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportClientActivity {
    pub rpc_client_id: u64,
    pub report: ClientActivityReport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveClientActivity {
    pub rpc_client_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ReadBackground;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateBackgroundPolicy {
    pub policy: BackgroundActivityPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ReadHostResources;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ReadProcessDiagnostics;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadProcessResourceHistory {
    pub window_ms: u64,
    pub bucket_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadTraceDiagnostics {
    pub trace_file_path: String,
    pub max_files: u32,
    pub slow_span_threshold_ms: Option<f64>,
}
