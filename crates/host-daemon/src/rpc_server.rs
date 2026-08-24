use std::{future::Future, sync::Arc, time::Duration};

use host_protocol::{
    RpcError, RpcId, RpcMessage, RpcNotification, RpcOutcome, RpcRequest, RpcResponse, read_frame,
    write_frame,
};
use serde_json::json;
use tokio::{
    sync::{Semaphore, mpsc},
    task::JoinSet,
    time::timeout,
};

use crate::{RpcChannel, RpcServerConfig, TransportError};

/// Serves an authenticated channel as a lossless Codex RPC gateway.
///
/// The `supported_methods` field in [`RpcServerConfig`] is only a capability
/// hint sent during the handshake.  It is deliberately not enforced here:
/// the installed Codex App Server remains the authority for which methods are
/// valid, and newer methods must be able to pass through without a Host
/// update.
///
/// Requests from the peer are handled concurrently with bounded timeout and
/// semaphore limits. Responses from the peer are delivered separately; this
/// lets a Codex server request make a complete round trip through the Host.
#[cfg(test)]
pub(crate) async fn serve_messages<RH, RF, SH, SF>(
    channel: RpcChannel,
    config: &RpcServerConfig,
    request_handler: RH,
    response_handler: SH,
    outbound_messages: mpsc::Receiver<RpcMessage>,
) -> Result<(), TransportError>
where
    RH: Fn(RpcRequest) -> RF + Send + Sync + 'static,
    RF: Future<Output = RpcOutcome> + Send + 'static,
    SH: Fn(RpcResponse) -> SF + Send + Sync + 'static,
    SF: Future + Send + 'static,
{
    let request_handler = Arc::new(request_handler);
    serve_message_loop(
        channel,
        config,
        move |request| {
            let request_handler = request_handler.clone();
            async move { RequestHandling::Respond(request_handler(request).await) }
        },
        response_handler,
        |_notification| async { false },
        outbound_messages,
    )
    .await
}

/// Serves a raw Codex session. The service owns request/response routing, so
/// request handlers do not produce an automatic response; the response is
/// delivered through the session's outbound queue instead. Notifications are
/// forwarded to the supplied handler as well.
pub async fn serve_gateway_messages<RH, RF, SH, SF, NH, NF>(
    channel: RpcChannel,
    config: &RpcServerConfig,
    request_handler: RH,
    response_handler: SH,
    notification_handler: NH,
    outbound_messages: mpsc::Receiver<RpcMessage>,
) -> Result<(), TransportError>
where
    RH: Fn(RpcRequest) -> RF + Send + Sync + 'static,
    RF: Future + Send + 'static,
    SH: Fn(RpcResponse) -> SF + Send + Sync + 'static,
    SF: Future + Send + 'static,
    NH: Fn(RpcNotification) -> NF + Send + Sync + 'static,
    NF: Future + Send + 'static,
{
    let request_handler = Arc::new(request_handler);
    let notification_handler = Arc::new(notification_handler);
    serve_message_loop(
        channel,
        config,
        move |request| {
            let request_handler = request_handler.clone();
            async move {
                request_handler(request).await;
                RequestHandling::Forwarded
            }
        },
        response_handler,
        move |notification| {
            let notification_handler = notification_handler.clone();
            async move {
                notification_handler(notification).await;
                true
            }
        },
        outbound_messages,
    )
    .await
}

async fn serve_message_loop<RH, RF, SH, SF, NH, NF>(
    channel: RpcChannel,
    config: &RpcServerConfig,
    request_handler: RH,
    response_handler: SH,
    notification_handler: NH,
    mut outbound_messages: mpsc::Receiver<RpcMessage>,
) -> Result<(), TransportError>
where
    RH: Fn(RpcRequest) -> RF + Send + Sync + 'static,
    RF: Future<Output = RequestHandling> + Send + 'static,
    SH: Fn(RpcResponse) -> SF + Send + Sync + 'static,
    SF: Future + Send + 'static,
    NH: Fn(RpcNotification) -> NF + Send + Sync + 'static,
    NF: Future<Output = bool> + Send + 'static,
{
    let (mut send, mut receive, max_frame_bytes) = channel.into_parts();
    let queue_capacity = usize::try_from(config.limits.outbound_queue_messages)
        .map_err(|_| TransportError::InvalidHandshake("outbound queue is too large".to_owned()))?;
    let concurrency = usize::try_from(config.limits.max_in_flight_requests).map_err(|_| {
        TransportError::InvalidHandshake("concurrency limit is too large".to_owned())
    })?;
    let request_timeout = Duration::from_millis(config.limits.request_timeout_ms);
    let permits = Arc::new(Semaphore::new(concurrency));
    let request_handler = Arc::new(request_handler);
    let response_handler = Arc::new(response_handler);
    let (outbound, mut outbound_rx) = mpsc::channel::<RpcMessage>(queue_capacity);
    let mut writer = tokio::spawn(async move {
        while let Some(message) = outbound_rx.recv().await {
            write_frame(&mut send, &message, max_frame_bytes).await?;
        }
        Ok::<(), TransportError>(())
    });
    let mut tasks = JoinSet::new();
    loop {
        while let Some(result) = tasks.try_join_next() {
            result.map_err(TransportError::RequestTask)?;
        }

        tokio::select! {
            outbound_message = outbound_messages.recv() => {
                let Some(message) = outbound_message else {
                    tasks.abort_all();
                    drop(outbound);
                    writer.abort();
                    return Err(TransportError::NotificationSourceClosed);
                };
                match outbound.try_send(message) {
                    Ok(()) => continue,
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        tasks.abort_all();
                        drop(outbound);
                        writer.abort();
                        return Err(TransportError::OutboundOverflow);
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        tasks.abort_all();
                        writer.abort();
                        return Err(TransportError::OutboundClosed);
                    }
                }
            }
            writer_result = &mut writer => {
                tasks.abort_all();
                drop(outbound);
                match writer_result {
                    Ok(result) => return result,
                    Err(error) => return Err(TransportError::RequestTask(error)),
                }
            }
            message = read_frame(&mut receive, max_frame_bytes) => {
                let message = match message {
                    Ok(message) => message,
                    Err(error) => {
                        tasks.abort_all();
                        drop(outbound);
                        writer.abort();
                        return Err(error.into());
                    }
                };
                match message {
                    RpcMessage::Request(request) => {
                        let Ok(permit) = permits.clone().try_acquire_owned() else {
                            send_outcome(
                                &outbound,
                                request.id,
                                RpcOutcome::Failure {
                                    error: rpc_error(
                                        "too_many_requests",
                                        "maximum in-flight request count reached",
                                    ),
                                },
                            )
                            .await?;
                            continue;
                        };

                        let outbound = outbound.clone();
                        let request_handler = request_handler.clone();
                        tasks.spawn(async move {
                            let id = request.id.clone();
                            let handling = match timeout(request_timeout, request_handler(request)).await {
                                Ok(handling) => handling,
                                Err(_) => RequestHandling::Respond(RpcOutcome::Failure {
                                    error: rpc_error(
                                        "request_timeout",
                                        "request exceeded its deadline",
                                    ),
                                }),
                            };
                            let _permit = permit;
                            if let RequestHandling::Respond(outcome) = handling {
                                let _ = send_outcome(&outbound, id, outcome).await;
                            }
                        });
                    }
                    RpcMessage::Response(response) => {
                        let response_handler = response_handler.clone();
                        tasks.spawn(async move {
                            response_handler(response).await;
                        });
                    }
                    RpcMessage::Notification(notification) => {
                        if !notification_handler(notification).await {
                            tasks.abort_all();
                            drop(outbound);
                            writer.abort();
                            return Err(TransportError::UnexpectedMessage);
                        }
                    }
                }
            }
        }
    }
}

enum RequestHandling {
    Respond(RpcOutcome),
    Forwarded,
}

async fn send_outcome(
    outbound: &mpsc::Sender<RpcMessage>,
    id: RpcId,
    outcome: RpcOutcome,
) -> Result<(), TransportError> {
    outbound
        .send(RpcMessage::Response(RpcResponse {
            id,
            outcome,
            extensions: Default::default(),
        }))
        .await
        .map_err(|_| TransportError::OutboundClosed)
}

fn rpc_error(code: &str, message: &str) -> RpcError {
    RpcError {
        code: json!(code),
        message: message.to_owned(),
        data: None,
        extensions: Default::default(),
    }
}
