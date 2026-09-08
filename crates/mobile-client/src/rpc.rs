use crate::MobileClientError;
use host_protocol::{
    HOST_REQUEST_LIMIT, RpcEventQueue, RpcPeer as JsonlPeer, RpcPeerError, raw_object,
};
use serde_json::{Value, value::RawValue};
use std::sync::Arc;
use std::{collections::BTreeMap, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::broadcast,
};

/// Authenticated mobile transport adapter; raw events share a single ordered queue.
pub(crate) struct RpcPeer {
    peer: Arc<JsonlPeer>,
    events: RpcEventQueue,
    request_timeout: Duration,
}
impl RpcPeer {
    pub(crate) fn open<S>(
        stream: S,
        max_message_bytes: usize,
        request_timeout: Duration,
    ) -> Result<Self, MobileClientError>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        if max_message_bytes == 0 {
            return Err(MobileClientError::InvalidConfig(
                "max_message_bytes must be positive",
            ));
        }
        let events = RpcEventQueue::new(4096);
        let sink = events.clone();
        let (reader, writer) = tokio::io::split(stream);
        let peer = JsonlPeer::open(
            reader,
            writer,
            max_message_bytes,
            HOST_REQUEST_LIMIT,
            move |event| sink.deliver(event),
        );
        Ok(Self {
            peer: Arc::new(peer),
            events,
            request_timeout,
        })
    }
    pub(crate) fn agent(&self) -> agent_client::operations::AgentClient {
        agent_client::operations::AgentClient::new(self.peer.clone(), self.request_timeout)
    }
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<String> {
        self.events.subscribe()
    }
    pub(crate) async fn request(
        &self,
        method: String,
        params: Value,
    ) -> Result<Value, MobileClientError> {
        self.request_raw(method, serde_json::to_string(&params)?)
            .await
    }
    pub(crate) async fn request_raw(
        &self,
        method: String,
        params: String,
    ) -> Result<Value, MobileClientError> {
        let line = request_line(0, &method, &params)?;
        let response = self
            .peer
            .request_raw(&line, self.request_timeout)
            .await
            .map_err(mobile_error)?;
        let object = raw_object(&response)?;
        if let Some(error) = object.get("error") {
            return Err(MobileClientError::Remote {
                error: error.get().to_owned(),
            });
        }
        let result = object
            .get("result")
            .ok_or_else(|| MobileClientError::Protocol("response has no result".into()))?;
        Ok(serde_json::from_str(result.get())?)
    }
    pub(crate) async fn respond_raw(
        &self,
        id: String,
        field: &'static str,
        payload: String,
    ) -> Result<(), MobileClientError> {
        if field != "result" && field != "error" {
            return Err(MobileClientError::Protocol(
                "response field must be result or error".into(),
            ));
        }
        self.peer
            .send_raw(&response_line(&id, field, &payload)?)
            .await
            .map_err(mobile_error)
    }
    pub(crate) fn close(&self) {
        self.peer.close();
    }
}
fn mobile_error(error: RpcPeerError) -> MobileClientError {
    match error {
        RpcPeerError::InvalidMessage(reason) => MobileClientError::Protocol(reason),
        RpcPeerError::ConnectionClosed(reason) => MobileClientError::Disconnected(reason),
        RpcPeerError::RequestTimeout { method } => MobileClientError::RequestTimeout { method },
        RpcPeerError::RequestIdExhausted => MobileClientError::RequestIdExhausted,
    }
}

fn request_line(id: u64, method: &str, params: &str) -> Result<String, MobileClientError> {
    #[derive(serde::Serialize)]
    struct Request<'a> {
        id: u64,
        method: &'a str,
        params: &'a RawValue,
    }
    Ok(serde_json::to_string(&Request {
        id,
        method,
        params: serde_json::from_str(params)?,
    })?)
}

fn response_line(
    id: &str,
    field: &'static str,
    payload: &str,
) -> Result<String, MobileClientError> {
    let id: &RawValue = serde_json::from_str(id)?;
    let payload: &RawValue = serde_json::from_str(payload)?;
    let mut object = BTreeMap::new();
    object.insert("id", id);
    object.insert(field, payload);
    Ok(serde_json::to_string(&object)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructed_request_does_not_change_nested_values() {
        let line = request_line(
            7,
            "codex/custom",
            r#"{"future": {"id": "nested"}, "text": "hello"}"#,
        )
        .unwrap();
        let object = raw_object(&line).unwrap();
        assert_eq!(
            object["params"].get(),
            r#"{"future": {"id": "nested"}, "text": "hello"}"#
        );
        let method = "custom/\"日本語\\method";
        let line = request_line(u64::MAX, method, "null").unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["id"], u64::MAX);
        assert_eq!(value["method"], method);
        assert!(value["params"].is_null());
        assert!(request_line(1, method, "{} []").is_err());
    }

    #[test]
    fn response_builder_keeps_raw_payload_and_id_values() {
        let line = response_line(
            r#""request-7""#,
            "result",
            r#"{"unknown":[1,{"future":true}]}"#,
        )
        .unwrap();
        let object = raw_object(&line).unwrap();
        assert_eq!(object["id"].get(), r#""request-7""#);
        assert_eq!(object["result"].get(), r#"{"unknown":[1,{"future":true}]}"#);
        let error = r#"{ "code": -1, "message": "失敗", "data": [null,{"id":7}] }"#;
        let line = response_line("7", "error", error).unwrap();
        let object = raw_object(&line).unwrap();
        assert_eq!(object["id"].get(), "7");
        assert_eq!(object["error"].get(), error);
        assert!(response_line("7 8", "result", "null").is_err());
        assert!(response_line("7", "error", "{").is_err());
    }
}
