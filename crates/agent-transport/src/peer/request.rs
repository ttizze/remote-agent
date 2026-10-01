//! Request preparation, wire IDs, and the cancellation-safe exchange lifecycle.
use super::envelope::request_line_with_id;
use super::io::{Termination, terminate};
use super::{
    Delivery, PeerError, Pending, Reply, RpcMessage, RpcMessageKind, RpcPeer, RpcResponse, invalid,
};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
};
use tokio::sync::oneshot;

struct PreparedRequest {
    original_id: Option<String>,
    method: String,
    id: u64,
    line: String,
}
pin_project_lite::pin_project! {
    /// A raw request's assigned wire ID is available before its first poll.
    /// Routers use it for notifications that refer to an outstanding request.
    pub struct Request<F> {
        pub(crate) wire_id: Option<u64>,
        #[pin]
        pub(crate) response: F,
    }
}
impl<F> Request<F> {
    /// `None` means preparation failed; awaiting the request returns that error.
    pub fn wire_id(&self) -> Option<u64> {
        self.wire_id
    }
}
impl<F: Future> Future for Request<F> {
    type Output = F::Output;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.project().response.poll(cx)
    }
}
impl<T, F: Future<Output = Result<Reply<RpcResponse<T>>, PeerError>> + Send> Request<F> {
    fn result(self) -> Request<impl Future<Output = Result<Reply<T>, PeerError>> + Send> {
        let response = self;
        Request {
            wire_id: response.wire_id(),
            response: async move {
                let reply = response.await?;
                Ok(Reply {
                    sequence: reply.sequence,
                    value: reply.value.outcome.map_err(|error| PeerError::Remote {
                        delivery: Delivery::from_error(&error),
                        error: error.get().into(),
                        sequence: Some(reply.sequence),
                    })?,
                })
            },
        }
    }
}
impl RpcPeer {
    /// Returns the original response line with only the caller-owned top-level ID restored.
    pub fn request_raw<'a>(
        &'a self,
        line: &str,
    ) -> Request<impl Future<Output = Result<Reply<String>, PeerError>> + Send + use<'a>> {
        let prepared = self.prepare(line);
        Request {
            wire_id: prepared.as_ref().ok().map(|request| request.id),
            response: async move { self.exchange(prepared?).await },
        }
    }
    fn prepare(&self, line: &str) -> Result<PreparedRequest, PeerError> {
        let message = RpcMessage::parse(line).map_err(invalid)?;
        if message.kind() != RpcMessageKind::Request {
            return Err(invalid("request must contain method and id"));
        }
        let original_id = message
            .raw_id()
            .ok_or_else(|| invalid("request ID is missing"))?
            .to_owned();
        let method = message
            .method()
            .ok_or_else(|| invalid("request method is missing"))?
            .to_owned();
        let id = allocate_id(&self.next_id)?;
        let line = message.rewrite_id(&id.to_string()).map_err(invalid)?;
        Ok(PreparedRequest {
            original_id: Some(original_id),
            method,
            id,
            line,
        })
    }
    async fn exchange(&self, prepared: PreparedRequest) -> Result<Reply<String>, PeerError> {
        let PreparedRequest {
            original_id,
            method,
            id,
            line,
        } = prepared;
        let expiration = async {
            match self.request_timeout {
                Some(timeout) => tokio::time::sleep(timeout).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(expiration);
        let _permit = tokio::select! {
            permit = self.permits.acquire() => permit.map_err(|_| self.closed_error())?,
            _ = &mut expiration => {
                tracing::error!(target: "bex", operation = %(&method), message = "RPC request timed out waiting for capacity");
                return Err(PeerError::RequestTimeout { method });
            },
        };
        let (tx, mut rx) = oneshot::channel();
        let received = {
            let mut state = self.state.lock().unwrap();
            if let Some(reason) = &state.closed {
                let error = PeerError::ConnectionClosed(reason.clone());
                drop(state);
                return Err(error);
            }
            state.pending.insert(
                id,
                Pending {
                    original_id,
                    method: Arc::from(method.as_str()),
                    complete: tx,
                },
            );
            state.sequence
        };
        let _cleanup = scopeguard::guard((&self.state, id), |(state, id)| {
            state.lock().unwrap().pending.remove(&id);
        });
        let response = tokio::select! {
            response = async {
                self.enqueue(line).await?;
                (&mut rx).await.map_err(|_| self.closed_error())
            } => Ok(response),
            _ = &mut expiration => Err(()),
        };
        match response {
            Ok(Ok(value)) => value,
            failure => {
                let error = match failure {
                    Ok(Err(error)) => error,
                    Err(_) => {
                        // Other replies or notifications prove the connection still works.
                        // A silent peer must release Store so clients can reconnect.
                        if self.state.lock().unwrap().sequence == received {
                            terminate(
                                &self.state,
                                &self.permits,
                                &self.stop,
                                Termination::Failed(format!(
                                    "no peer traffic before {method} timed out"
                                )),
                            );
                        } else {
                            tracing::error!(target: "bex", operation = %(&method), message = "RPC request timed out waiting for response");
                        }
                        PeerError::RequestTimeout { method }
                    }
                    _ => unreachable!(),
                };
                let pending = self.state.lock().unwrap().pending.remove(&id);
                if let Some(pending) = pending {
                    let _ = pending.complete.send(Err(error));
                }
                rx.await.map_err(|_| self.closed_error())?
            }
        }
    }
    /// Decode the provider result while preserving its error payload.
    pub fn request_envelope<'a, P: Serialize, T: DeserializeOwned>(
        &'a self,
        method: &'a str,
        params: &P,
    ) -> Request<
        impl Future<Output = Result<Reply<RpcResponse<T>>, PeerError>> + Send + use<'a, P, T>,
    > {
        let prepared = allocate_id(&self.next_id).and_then(|id| {
            Ok(PreparedRequest {
                original_id: None,
                method: method.into(),
                id,
                line: request_line_with_id(id, method, params)?,
            })
        });
        Request {
            wire_id: prepared.as_ref().ok().map(|request| request.id),
            response: async move {
                let reply = self.exchange(prepared?).await?;
                let value = RpcResponse::parse(&reply.value).map_err(|error| {
                    PeerError::InvalidResponse {
                        method: method.into(),
                        reason: error.to_string(),
                        raw: RpcMessage::parse(&reply.value)
                            .ok()
                            .and_then(|message| message.raw_result())
                            .map_or_else(|| reply.value.clone(), |result| result.get().into()),
                        sequence: Some(reply.sequence),
                    }
                })?;
                Ok(Reply {
                    sequence: reply.sequence,
                    value,
                })
            },
        }
    }

    pub fn request<'a, P: Serialize, T: DeserializeOwned>(
        &'a self,
        method: &'a str,
        params: &P,
    ) -> Request<impl Future<Output = Result<Reply<T>, PeerError>> + Send + use<'a, P, T>> {
        let response = self.request_envelope(method, params);
        response.result()
    }
}

fn allocate_id(next: &AtomicU64) -> Result<u64, PeerError> {
    next.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .map_err(|_| PeerError::RequestIdExhausted)
}
