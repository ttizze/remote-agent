use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::CodexRpcService;
use host_protocol::{
    DEFAULT_MAX_MESSAGE_BYTES, JsonlReader, JsonlWriter, RpcMessageKind, classify_message,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, split},
    sync::Semaphore,
    task::JoinSet,
};

const MAX_IN_FLIGHT_REQUESTS: usize = 8;

pub(crate) async fn serve_jsonl_session<S>(
    stream: S,
    service: CodexRpcService,
    disconnect: CancellationToken,
    mut session: crate::CodexSession,
) -> Result<(), String>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let session_id = session.id();
    let (reader, writer) = split(stream);
    let mut reader = JsonlReader::with_max_message_bytes(reader, DEFAULT_MAX_MESSAGE_BYTES);
    let mut writer = JsonlWriter::with_max_message_bytes(writer, DEFAULT_MAX_MESSAGE_BYTES);
    let permits = Arc::new(Semaphore::new(MAX_IN_FLIGHT_REQUESTS));
    let mut tasks = JoinSet::<Result<(), String>>::new();

    let result = async { loop {
        let mut task_error = None;
        while let Some(result) = tasks.try_join_next() {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    task_error = Some(error);
                    break;
                }
                Err(error) => {
                    task_error = Some(format!("request task failed: {error}"));
                    break;
                }
            }
        }
        if let Some(error) = task_error {
            break Err(error);
        }

        tokio::select! {
            biased;
            _ = disconnect.cancelled() => break Ok(()),
            incoming = reader.read_line() => {
                let Some(line) = incoming.map_err(|error| error.to_string())? else {
                    break Ok(());
                };
                let message = classify_message(&line)
                    .map_err(|error| format!("invalid JSONL message: {error}"))?;
                match message.kind() {
                    RpcMessageKind::Request => {
                        let Ok(permit) = permits.clone().try_acquire_owned() else {
                            break Err("maximum in-flight request count reached".to_owned());
                        };
                        let service = service.clone();
                        tasks.spawn(async move {
                            let _permit = permit;
                            service
                                .dispatch_request(session_id, line)
                                .await
                                .map_err(|error| error.to_string())
                        });
                    }
                    RpcMessageKind::Response => {
                        service
                            .dispatch_response(session_id, line)
                            .await
                            .map_err(|error| error.to_string())?;
                    }
                    RpcMessageKind::Notification => {
                        service
                            .dispatch_notification(session_id, line)
                            .await
                            .map_err(|error| error.to_string())?;
                    }
                }
            }
            outgoing = session.recv() => {
                let Some(line) = outgoing else {
                    break Err("session outbound queue closed".to_owned());
                };
                writer.write_line(&line).await.map_err(|error| error.to_string())?;
            }
        }
    }}.await;

    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    service.close_session(session_id);
    result
}
