//! Identifiers crossing the native ABI retain their validated domain types.
use uuid::Uuid;
uniffi::custom_type!(Uuid, String, {remote, lower: |value| value.to_string(), try_lift: |value| Ok(value.parse()?) });
