//! Bex requests own independent QUIC streams. Provider JSONL state stays in `peer`.
use crate::{
    peer::{Delivery, PeerError},
    protocol::{self, Call, Response},
};
use serde::de::DeserializeOwned;
use std::time::Duration;
use tokio::sync::Semaphore;
pub(crate) const EVENTS: u8 = 0;
pub(crate) const CALL: u8 = 1;
pub(crate) const BLOB: u8 = 2;
pub(crate) const CLOSE: u8 = 3;
pub type Updates = protocol::Reader;
pub type HostPeer = iroh::endpoint::SendStream;
pub struct HostRequest {
    pub call: Call,
    pub send: iroh::endpoint::SendStream,
}
pub struct Client {
    connection: iroh::endpoint::Connection,
    permits: Semaphore,
    timeout: Duration,
}
impl Client {
    pub(crate) async fn connect(
        connection: iroh::endpoint::Connection,
        timeout: Duration,
        max_requests: usize,
    ) -> Result<(Self, Updates), PeerError> {
        if max_requests == 0 {
            return Err(invalid("max_requests must be positive"));
        }
        let (mut send, recv) = connection.open_bi().await.map_err(invalid)?;
        send.write_all(&[EVENTS]).await.map_err(invalid)?;
        send.finish().map_err(invalid)?;
        Ok((
            Self {
                connection,
                permits: Semaphore::new(max_requests),
                timeout,
            },
            protocol::Reader::new(recv),
        ))
    }
    pub async fn close(&self) {
        // Observe shutdown before a short-lived CLI exits its runtime.
        tokio::select! {
            _ = self.connection.closed() => {},
            _ = tokio::time::timeout(Duration::from_secs(3), async {
                let (mut send, mut recv) = self.connection.open_bi().await.map_err(invalid)?;
                send.write_all(&[CLOSE]).await.map_err(invalid)?;
                send.finish().map_err(invalid)?;
                recv.read_exact(&mut [0u8; 1]).await.map_err(invalid)?;
                Ok::<(), PeerError>(())
            }) => {},
        }
        self.connection.close(0u8.into(), b"peer closed");
    }
    pub async fn request<T: DeserializeOwned>(&self, call: &Call) -> Result<T, PeerError> {
        self.request_stream(call).await.map(|(result, _)| result)
    }
    pub async fn request_stream<T: DeserializeOwned>(
        &self,
        call: &Call,
    ) -> Result<(T, Updates), PeerError> {
        let (initial, updates) = self.call_stream(call).await?;
        Ok((
            response(protocol::decode(&initial).map_err(invalid)?)?,
            updates,
        ))
    }
    async fn call_stream(
        &self,
        call: &Call,
    ) -> Result<(tokio_util::bytes::BytesMut, Updates), PeerError> {
        let method = call.method().to_owned();
        let work = async {
            let _permit = self.permits.acquire().await.map_err(invalid)?;
            let (mut send, recv) = self.connection.open_bi().await.map_err(invalid)?;
            send.write_all(&[CALL]).await.map_err(invalid)?;
            protocol::write(&mut send, call).await.map_err(invalid)?;
            send.finish().map_err(invalid)?;
            let mut replies = protocol::Reader::new(recv);
            let initial = replies
                .read_frame()
                .await
                .map_err(invalid)?
                .ok_or_else(|| invalid("response stream ended before its result"))?;
            Ok((initial, replies))
        };
        tokio::select! {
            reason = self.connection.closed() => Err(PeerError::ConnectionClosed(reason.to_string())),
            result = tokio::time::timeout(self.timeout, work) => result.unwrap_or_else(|_| Err(PeerError::RequestTimeout {method})),
        }
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.connection.close(0u8.into(), b"peer dropped");
    }
}
fn response<T>(response: Response<T>) -> Result<T, PeerError> {
    match response {
        Response::Success { result } => Ok(result),
        Response::Failure { error } => Err(PeerError::Remote {
            delivery: serde_json::from_value(error["delivery"].clone())
                .unwrap_or(Delivery::Unknown),
            error: error.to_string(),
            sequence: None,
        }),
    }
}
fn invalid(error: impl std::fmt::Display) -> PeerError {
    PeerError::InvalidMessage(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{Endpoint, Identity, IncomingRequest, Relays, Trust};

    #[tokio::test]
    async fn unread_response_cancellation_and_timeout_do_not_block_other_calls() {
        tokio::time::timeout(Duration::from_secs(15), async {
            let host = Endpoint::bind(Identity::generate(), Relays::Disabled)
                .await
                .unwrap();
            let client = Endpoint::bind(Identity::generate(), Relays::Disabled)
                .await
                .unwrap();
            let trust = Trust {
                allowed: [client.node_id()].into(),
                ..Default::default()
            };
            let ticket = host.ticket();
            let (session, incoming) = tokio::join!(client.connect(&ticket), host.accept());
            let session = session.unwrap();
            let incoming = incoming.unwrap().unwrap().authorize(&trust).unwrap();
            let (remote, host_peer) = tokio::join!(
                session.open_peer(Duration::from_secs(3), 8),
                incoming.accept_peer()
            );
            let (remote, mut events) = remote.unwrap();
            let host_peer = host_peer.unwrap();
            let serving = tokio::spawn(async move {
                let mut host_peer = host_peer;
                for code in 0..5000 {
                    protocol::write(
                        &mut host_peer,
                        protocol::Notification::Exited {
                            handle: "terminal".into(),
                            code,
                        },
                    )
                    .await
                    .unwrap();
                }
                let mut requests = tokio::task::JoinSet::new();
                loop {
                    match incoming.accept_request().await.unwrap() {
                        IncomingRequest::Call(request) => {
                            requests.spawn(async move {
                                let mut send = request.send;
                                if request.call.method() == "malformed" {
                                    let _ = protocol::write_frame(&mut send, &[0]).await;
                                    return;
                                }
                                if request.call.method() == "silent" {
                                    let _ = send.stopped().await;
                                    return;
                                }
                                let body = if request.call.method() == "large" {
                                    "x".repeat(8 * 1024 * 1024)
                                } else {
                                    "pong".into()
                                };
                                let _ = crate::protocol::write(
                                    &mut send,
                                    crate::protocol::Response::Success { result: body },
                                )
                                .await;
                            });
                        }
                        IncomingRequest::Close => break,
                        IncomingRequest::Blob(_) => panic!("unexpected blob"),
                    }
                }
                requests.abort_all();
                while requests.join_next().await.is_some() {}
            });
            let (mut send, mut recv) = remote.connection.open_bi().await.unwrap();
            send.write_all(&[CALL]).await.unwrap();
            crate::protocol::write(
                &mut send,
                crate::protocol::Call::Provider(crate::protocol::ProviderCall {
                    method: "large".into(),
                    params: serde_json::json!({}),
                }),
            )
            .await
            .unwrap();
            send.finish().unwrap();
            recv.read_exact(&mut [0u8; 1]).await.unwrap();
            assert_eq!(
                remote
                    .request::<String>(&crate::protocol::Call::Provider(
                        crate::protocol::ProviderCall {
                            method: "ping".into(),
                            params: serde_json::json!({})
                        }
                    ))
                    .await
                    .unwrap(),
                "pong"
            );
            drop((send, recv));
            assert!(
                remote
                    .request::<String>(&crate::protocol::Call::Provider(
                        crate::protocol::ProviderCall {
                            method: "malformed".into(),
                            params: serde_json::json!({})
                        }
                    ))
                    .await
                    .is_err()
            );
            assert!(matches!(
                remote
                    .request::<String>(&crate::protocol::Call::Provider(
                        crate::protocol::ProviderCall {
                            method: "silent".into(),
                            params: serde_json::json!({})
                        }
                    ))
                    .await,
                Err(PeerError::RequestTimeout { .. })
            ));
            assert_eq!(
                remote
                    .request::<String>(&crate::protocol::Call::Provider(
                        crate::protocol::ProviderCall {
                            method: "ping".into(),
                            params: serde_json::json!({})
                        }
                    ))
                    .await
                    .unwrap(),
                "pong"
            );
            // A slow consumer keeps every event in order, beyond the former
            // broadcast capacity, while independent RPCs keep completing.
            for expected in 0..5000 {
                let Some(protocol::Notification::Exited { code, .. }) =
                    events.read().await.unwrap()
                else {
                    panic!("missing terminal event")
                };
                assert_eq!(code, expected);
            }
            remote.close().await;
            serving.await.unwrap();
            client.close().await;
            host.close().await;
        })
        .await
        .unwrap();
    }
}
