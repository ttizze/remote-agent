//! Validated identities and timestamps; no clock or random source.
use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! ids {
    ($($name:ident),+ $(,)?) => {$ (
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);
        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, ContractError> {
                let value = value.into();
                if value.is_empty() || value.trim() != value {
                    return Err(ContractError::InvalidId);
                }
                Ok(Self(value))
            }
            pub fn as_str(&self) -> &str { &self.0 }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { self.0.fmt(f) }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                Self::new(String::deserialize(d)?).map_err(serde::de::Error::custom)
            }
        }
    )+};
}
ids!(
    ThreadId,
    CommandId,
    MessageId,
    RunId,
    RunAttemptId,
    TurnItemId,
    RuntimeRequestId,
    PlanId,
    CheckpointId,
    ContextTransferId,
    NodeId
);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContractError {
    #[error("id must be a nonempty trimmed string")]
    InvalidId,
    #[error("timestamp must be RFC 3339")]
    InvalidTimestamp,
}

/// Normalize timestamps so lexical ordering agrees with chronological ordering.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Timestamp(String);
impl Timestamp {
    pub fn parse(value: &str) -> Result<Self, ContractError> {
        let time = chrono::DateTime::parse_from_rfc3339(value)
            .map_err(|_| ContractError::InvalidTimestamp)?;
        Self::from_millis(time.timestamp_millis())
    }
    pub fn from_millis(value: i64) -> Result<Self, ContractError> {
        use chrono::Datelike;
        let time = chrono::DateTime::from_timestamp_millis(value)
            .filter(|time| (0..=9999).contains(&time.year()))
            .ok_or(ContractError::InvalidTimestamp)?;
        Ok(Self(
            time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        ))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn millis(&self) -> i64 {
        chrono::DateTime::parse_from_rfc3339(&self.0)
            .expect("validated timestamp")
            .timestamp_millis()
    }
}
impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// JSON is open only at provider/tool boundaries; Postcard carries a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Json(pub serde_json::Value);
impl Serialize for Json {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            self.0.serialize(s)
        } else {
            serde_json::to_string(&self.0)
                .map_err(serde::ser::Error::custom)?
                .serialize(s)
        }
    }
}
impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        if d.is_human_readable() {
            serde_json::Value::deserialize(d).map(Self)
        } else {
            serde_json::from_str(&String::deserialize(d)?)
                .map(Self)
                .map_err(serde::de::Error::custom)
        }
    }
}
