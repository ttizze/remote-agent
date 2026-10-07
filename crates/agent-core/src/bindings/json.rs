//! Identifiers crossing the native ABI retain their validated domain types.
use uuid::Uuid;
uniffi::custom_type!(Uuid, String, {remote, lower: |value| value.to_string(), try_lift: |value| Ok(value.parse()?) });
/// The domain `ThreadId` owns the native name; this one crosses as the same string.
type ProtocolThreadId = agent_protocol::orchestration::ThreadId;
uniffi::custom_type!(ProtocolThreadId, String, {remote, lower: |value| value.to_string(), try_lift: |value| Ok(ProtocolThreadId::new(value)?) });
