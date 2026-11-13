//! Named MessagePack records for Host RPC. A QUIC stream owns each reply.
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::io;

#[cfg(any(test, feature = "test-support"))]
pub mod json_boundary;
mod requests;
pub use requests::{Call, contracts};

pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method")]
pub enum Notification {
    #[serde(rename = "host/provider/settingsChanged")]
    ProviderSettings {
        settings: crate::providers::ProviderSettings,
    },
    #[serde(rename = "host/terminal/output")]
    Output {
        #[serde(rename = "processHandle")]
        handle: String,
        #[serde(rename = "deltaBase64", with = "bytes")]
        data: Vec<u8>,
    },
    #[serde(rename = "host/terminal/exited")]
    Exited {
        #[serde(rename = "processHandle")]
        handle: String,
        #[serde(rename = "exitCode")]
        code: i32,
    },
    #[serde(rename = "host/terminal/failed")]
    TerminalFailed {
        #[serde(rename = "processHandle")]
        handle: String,
        #[serde(rename = "message")]
        reason: String,
    },
    #[serde(rename = "host/terminal/restored")]
    TerminalRestored {
        #[serde(rename = "processHandle")]
        handle: String,
        #[serde(rename = "deltaBase64", with = "bytes")]
        data: Vec<u8>,
        cols: u16,
        rows: u16,
    },
    #[serde(rename = "host/terminal/detached")]
    TerminalDetached {
        #[serde(rename = "processHandle")]
        handle: String,
    },
    #[serde(other)]
    Unsupported,
}

pub fn encode(value: impl Serialize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    encode_into(&mut bytes, &value)?;
    Ok(bytes)
}
pub(crate) fn encode_into(writer: impl io::Write, value: &impl Serialize) -> io::Result<()> {
    value
        .serialize(&mut rmp_serde::Serializer::new(writer).with_struct_map())
        .map_err(io::Error::other)
}
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> io::Result<T> {
    let mut remaining = bytes;
    let value = T::deserialize(&mut rmp_serde::Deserializer::new(&mut remaining))
        .map_err(io::Error::other)?;
    if !remaining.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "trailing bytes in frame",
        ));
    }
    Ok(value)
}
pub fn response_frame(response: Response) -> io::Result<Vec<u8>> {
    let bytes = encode(response)?;
    if bytes.len() <= MAX_FRAME_BYTES {
        return Ok(bytes);
    }
    encode(Response::<()>::Failure {
        error: crate::error::RpcFailure {
            code: "response_too_large".into(),
            message: "RPC response exceeds the transfer limit".into(),
            delivery: crate::error::Delivery::Unknown,
            execution: None,
        },
    })
}
// Host handlers return domain results; each serializes directly.
macro_rules! results {
    ($($variant:ident($(#[$attr:meta])* $ty:ty)),* $(,)?) => {
        #[derive(Debug, Serialize)]
        #[serde(untagged)]
        pub enum Body {$($variant($(#[$attr])* Box<$ty>)),*}
        $(impl From<$ty> for Body {fn from(value:$ty)->Self {Self::$variant(Box::new(value))}})*
    }
}
results! {
    List(crate::conversation::records::OpenedList),
    CommandOutput(crate::conversation::records::OpenedOutput),
    ConversationHydration(crate::conversation::records::Hydrated),
    ConversationReceipts(crate::conversation::records::Receipts),
    ProviderSettings(crate::providers::ProviderSettings),
    HostSoftware(crate::operations::HostSoftware),
    FileMetadataEntries(crate::file_metadata::FileMetadataEntries),
    HostUpdate(crate::operations::HostUpdate), HostIdentity(crate::operations::HostIdentity),
    HandoffHistory(crate::handoff::Page),
    LiveActivity(crate::live_activity::LiveActivityRegistration),
    Browser(crate::browser::BrowserFrame),
    Conversation(crate::conversation::records::Opened), ConversationAck(crate::conversation::records::Acknowledged), ConversationRows(Vec<crate::conversation::records::Message>),
    Threads(crate::models::ThreadList),
    Decorations(crate::operations::ListDecorations),
    ProjectScan(crate::project_import::ProjectScan),
    ProjectImport(crate::project_import::ProjectImportResult),
    Agents(Vec<crate::models::AgentObservation>),
    ComposerCatalog(crate::composer::ComposerCatalog),
    FileOpenCapabilities(crate::editors::FileOpenCapabilities),
    TreeEntries(crate::file_tree::TreeEntries),
    PermissionSettings(crate::permissions::PermissionSettings),
    Models(crate::operations::ModelPage), WorktreeSettings(crate::models::WorktreeSettings),
    Worktrees(Vec<crate::models::Worktree>), Review(crate::models::WorkspaceReview),
    Session(crate::session::SessionRef), Empty(crate::models::Empty),
    AccountUsage(crate::operations::AccountUsage),
    Accounts(crate::operations::Accounts), Selected(crate::operations::AccountSelection),
    ModelSettings(crate::models::ModelSettings),
    DshCredentials(crate::dsh::Credentials),
    Login(crate::operations::AccountLogin), LoginStatus(crate::operations::AccountLoginStatus),
    Files(crate::models::FileList), FileEntry(crate::models::FileEntry), File(crate::models::FileContent), Grant(crate::models::TransferGrant),
    Transcription(crate::operations::Transcription),
    HostStatus(crate::models::HostStatus), Invitation(crate::models::Invitation),
    Remotes(Vec<crate::models::RemoteHost>), Remote(crate::models::RemoteHost),
    Unit(()), Text(String), CodexCli(crate::operations::CodexCliStatus)
}
#[derive(Debug, Serialize, Deserialize)]
pub enum Response<T = Body> {
    Success { result: T },
    Failure { error: crate::error::RpcFailure },
}
impl Response {
    pub fn from_result<T: Into<Body>, E: Into<crate::error::RpcFailure>>(
        result: Result<T, E>,
    ) -> Response {
        match result {
            Ok(result) => Response::Success {
                result: result.into(),
            },
            Err(error) => Response::Failure {
                error: error.into(),
            },
        }
    }
    pub fn error(code: impl ToString, message: &impl std::fmt::Display) -> Response {
        Self::from_result::<(), _>(Err(crate::error::RpcFailure {
            code: code.to_string(),
            message: message.to_string(),
            delivery: crate::error::Delivery::Unknown,
            execution: None,
        }))
    }
}
impl<T: Serialize> Response<T> {
    /// Used only by JSON CLI output and provider fixtures.
    pub fn into_value(self) -> Value {
        match self {
            Self::Success { result } => serde_json::json!({"result":result}),
            Self::Failure { error } => serde_json::json!({"error":error}),
        }
    }
}

/// JSON APIs require base64; QUIC transports the original bytes.
pub mod bytes {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use serde::{
        Deserializer, Serializer,
        de::{Error, Visitor},
    };
    pub fn serialize<T: AsRef<[u8]> + ?Sized, S: Serializer>(
        value: &T,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            serializer.collect_str(&base64::display::Base64Display::new(
                value.as_ref(),
                &STANDARD,
            ))
        } else {
            serializer.serialize_bytes(value.as_ref())
        }
    }
    pub fn deserialize<'de, T: From<Vec<u8>>, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<T, D::Error> {
        // Serde buffers tagged-enum fields and loses is_human_readable().
        // The value itself distinguishes JSON base64 from MessagePack binary.
        struct Bytes;
        impl<'de> Visitor<'de> for Bytes {
            type Value = Vec<u8>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("binary bytes or a base64 string")
            }
            fn visit_str<E: Error>(self, value: &str) -> Result<Self::Value, E> {
                STANDARD.decode(value).map_err(E::custom)
            }
            fn visit_bytes<E: Error>(self, value: &[u8]) -> Result<Self::Value, E> {
                Ok(value.to_vec())
            }
            fn visit_byte_buf<E: Error>(self, value: Vec<u8>) -> Result<Self::Value, E> {
                Ok(value)
            }
        }
        deserializer.deserialize_any(Bytes).map(T::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;

    // Independent peer schemas deliberately reorder fields/variants and add data.
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(tag = "method")]
    enum NewClient {
        #[serde(rename = "host/future/operation")]
        Future { nested: Value },
        #[serde(rename = "host/model/list")]
        List {
            provider: crate::session::ProviderKind,
            #[serde(default)]
            include_hidden: bool,
            cursor: Option<String>,
            limit: usize,
        },
    }

    proptest! {
        #[test]
        fn terminal_bytes_stay_binary_and_survive_named_event_envelopes(data in proptest::collection::vec(any::<u8>(), 1..512)) {
            let bytes = encode(Notification::Output { handle:"terminal".into(), data:data.clone() }).unwrap();
            let decoded: Notification = decode(&bytes).unwrap();
            prop_assert!(matches!(decoded, Notification::Output {handle, data: actual} if handle == "terminal" && actual == data), "terminal bytes or identity changed");
            prop_assert!(bytes.windows(data.len()).any(|chunk| chunk == data));
            let call = Call::Transcribe(crate::operations::Transcribe {preparation:None, audio:data.clone()});
            let decoded: Call = decode(&encode(call).unwrap()).unwrap();
            prop_assert!(matches!(decoded, Call::Transcribe(params) if params.audio == data), "request bytes changed");
        }

        #[test]
        fn independently_updated_peers_use_names_not_positions(limit in 0usize..1000, cursor in proptest::option::of(".{0,50}"), hidden in any::<bool>()) {
            let newer = NewClient::List { provider: crate::session::ProviderKind::Codex, include_hidden: hidden, cursor: cursor.clone(), limit };
            let decoded: Call = decode(&encode(&newer).unwrap()).unwrap();
            prop_assert!(matches!(&decoded, Call::ListModels(params) if params.limit == limit && params.cursor == cursor));
            let NewClient::List { include_hidden, cursor: actual_cursor, limit: actual_limit, provider } = decode(&encode(decoded).unwrap()).unwrap() else {
                return Err(TestCaseError::fail("method changed when the peer reordered its enum"));
            };
            prop_assert_eq!(provider, crate::session::ProviderKind::Codex);
            prop_assert!(!include_hidden);
            prop_assert_eq!(actual_cursor, cursor);
            prop_assert_eq!(actual_limit, limit);
        }
    }

    #[test]
    fn request_names_and_missing_optional_fields_are_wire_contracts() {
        let call: Call = decode(
            &encode(json!({
                "method":"host/model/list", "provider":"codex", "limit":10
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(matches!(
            call,
            Call::ListModels(crate::operations::ListModels {
                provider: crate::session::ProviderKind::Codex,
                limit: 10,
                cursor: None
            })
        ));
        let wire: Value = decode(&encode(call).unwrap()).unwrap();
        assert_eq!(wire["method"], "host/model/list");
        assert_eq!(wire["limit"], 10);
        let invitation = uuid::Uuid::from_bytes([7; 16]);
        assert!(
            matches!(decode::<Call>(&encode(Call::Pair(crate::operations::Pair { invitation })).unwrap()).unwrap(), Call::Pair(params) if params.invitation == invitation)
        );
        assert!(
            decode::<Call>(
                &encode(json!({
                    "method":"host/model/list", "limit":"invalid"
                }))
                .unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn new_operations_and_notifications_have_explicit_fallbacks() {
        let newer = NewClient::Future {
            nested: json!({"new":[1,true,{"text":"future"}]}),
        };
        assert!(matches!(
            decode::<Call>(&encode(newer).unwrap()).unwrap(),
            Call::Unsupported
        ));
        let future =
            encode(json!({"method":"host/future/event","params":{"new":[1,true]}})).unwrap();
        assert!(matches!(
            decode::<Notification>(&future).unwrap(),
            Notification::Unsupported
        ));
        // Missing/invalid envelopes are errors, not ignorable future events.
        assert!(decode::<Notification>(&encode(json!({"params":{}})).unwrap()).is_err());
        assert!(
            decode::<Notification>(
                &encode(json!({"method":"host/terminal/exited","exitCode":"invalid"})).unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn snapshots_accept_new_fields_and_unknown_status_without_losing_content() {
        let bytes = encode(json!({
            "Success":{"result":{
                "id":{"provider":"codex","id":"thread"},
                "status":"future-status", "futureField":{"nested":true},
                "capabilities":{"fork":true,"futureCapability":true},
                "turns":[{"id":"turn","status":"future-status","items":[{
                    "id":"item","status":"future-status",
                    "body":{"inline":{"body":{"assistantText":{"text":"kept","phase":"future-phase"}}}}
                }]}]
            }}
        })).unwrap();
        let Response::Success { result: thread } =
            decode::<Response<crate::models::Thread>>(&bytes).unwrap()
        else {
            panic!("expected snapshot")
        };
        assert_eq!(thread.status, crate::execution::SessionStatus::Unknown);
        assert!(thread.capabilities.unwrap().fork);
        assert!(!thread.capabilities.unwrap().steer);
        let turn = &thread.turns.unwrap()[0];
        assert_eq!(turn.status, crate::execution::TurnStatus::Unknown);
        let item = &turn.items.as_ref().unwrap()[0];
        assert_eq!(item.status, crate::execution::ItemStatus::Unknown);
        assert!(
            matches!(item.body(), crate::items::ItemBody::AssistantText {text,phase:crate::items::AssistantPhase::Unknown} if text == "kept")
        );
    }

    #[test]
    fn unknown_session_updates_preserve_the_timeline() {
        let change: crate::session::SessionChange = decode(
            &encode(json!({
                "type":"futureChange", "data":{"turnId":"future","new":true}
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(matches!(change, crate::session::SessionChange::Unsupported));
        let original = crate::session::Timeline {
            status: crate::execution::SessionStatus::Running,
            ..Default::default()
        };
        let (actual, result) = change.apply_timeline(original.clone());
        result.unwrap();
        assert_eq!(actual, original);
    }

    #[test]
    fn frame_decoding_rejects_truncation_and_trailing_messages() {
        let bytes = encode(Call::HostName(crate::models::Empty {})).unwrap();
        for end in 0..bytes.len() {
            assert!(decode::<Call>(&bytes[..end]).is_err(), "truncated at {end}");
        }
        let mut trailing = bytes;
        trailing.extend(encode(1).unwrap());
        assert!(decode::<Call>(&trailing).is_err());
    }
}
