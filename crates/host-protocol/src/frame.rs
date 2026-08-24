use std::{io, io::ErrorKind};

use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame length {declared} exceeds maximum {maximum}")]
    TooLarge { declared: u32, maximum: u32 },
    #[error("empty frames are not valid RPC messages")]
    Empty,
    #[error("RPC message cannot be represented in a 32-bit frame length")]
    LengthOverflow,
    #[error("invalid RPC JSON: {0}")]
    Json(#[from] serde_json::Error),
}

pub async fn read_frame<R, T>(reader: &mut R, maximum: u32) -> Result<T, FrameError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut header = [0_u8; 4];
    reader.read_exact(&mut header).await?;
    let declared = u32::from_be_bytes(header);
    if declared == 0 {
        return Err(FrameError::Empty);
    }
    if declared > maximum {
        return Err(FrameError::TooLarge { declared, maximum });
    }

    let mut payload = vec![0_u8; declared as usize];
    reader.read_exact(&mut payload).await.map_err(|error| {
        if error.kind() == ErrorKind::UnexpectedEof {
            io::Error::new(
                ErrorKind::UnexpectedEof,
                format!("frame declared {declared} bytes but payload ended early"),
            )
        } else {
            error
        }
    })?;

    Ok(serde_json::from_slice(&payload)?)
}

pub async fn write_frame<W, T>(writer: &mut W, value: &T, maximum: u32) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let payload = serde_json::to_vec(value)?;
    let declared = u32::try_from(payload.len()).map_err(|_| FrameError::LengthOverflow)?;
    if declared == 0 {
        return Err(FrameError::Empty);
    }
    if declared > maximum {
        return Err(FrameError::TooLarge { declared, maximum });
    }

    writer.write_all(&declared.to_be_bytes()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::{RpcError, RpcId, RpcMessage, RpcOutcome, RpcRequest, RpcResponse};
    use serde_json::json;
    use tokio::io::{AsyncWriteExt, duplex};

    #[tokio::test]
    async fn round_trips_rpc_message_with_big_endian_length() {
        let request = RpcMessage::Request(RpcRequest {
            id: RpcId::Integer(42),
            method: "turn/start".to_owned(),
            params: json!({ "text": "hello" }),
            extensions: Default::default(),
        });
        let expected_payload = serde_json::to_vec(&request).unwrap();
        let (mut writer, mut reader) = duplex(8 * 1024);

        write_frame(&mut writer, &request, 1024).await.unwrap();

        let mut header = [0_u8; 4];
        reader.read_exact(&mut header).await.unwrap();
        assert_eq!(
            u32::from_be_bytes(header),
            u32::try_from(expected_payload.len()).unwrap()
        );
        let mut payload = vec![0; expected_payload.len()];
        reader.read_exact(&mut payload).await.unwrap();
        assert_eq!(payload, expected_payload);

        let decoded: RpcMessage = serde_json::from_slice(&payload).unwrap();
        assert_eq!(decoded, request);
    }

    #[tokio::test]
    async fn read_frame_round_trips_response() {
        let response = RpcMessage::Response(RpcResponse {
            id: RpcId::Integer(7),
            outcome: RpcOutcome::Failure {
                error: RpcError {
                    code: json!("unknown_method"),
                    message: "unsupported".to_owned(),
                    data: None,
                    extensions: Default::default(),
                },
            },
            extensions: Default::default(),
        });
        let (mut writer, mut reader) = duplex(8 * 1024);

        write_frame(&mut writer, &response, 1024).await.unwrap();
        let decoded: RpcMessage = read_frame(&mut reader, 1024).await.unwrap();

        assert_eq!(decoded, response);
    }

    #[tokio::test]
    async fn rejects_oversized_length_before_reading_payload() {
        let (mut writer, mut reader) = duplex(64);
        writer.write_all(&1025_u32.to_be_bytes()).await.unwrap();

        let error = read_frame::<_, RpcMessage>(&mut reader, 1024)
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            FrameError::TooLarge {
                declared: 1025,
                maximum: 1024
            }
        ));
    }

    #[tokio::test]
    async fn reports_truncated_payload() {
        let (mut writer, mut reader) = duplex(64);
        writer.write_all(&10_u32.to_be_bytes()).await.unwrap();
        writer.write_all(b"{}").await.unwrap();
        writer.shutdown().await.unwrap();

        let error = read_frame::<_, RpcMessage>(&mut reader, 1024)
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            FrameError::Io(ref source)
                if source.kind() == ErrorKind::UnexpectedEof
                    && source.to_string().contains("declared 10 bytes")
        ));
    }

    #[tokio::test]
    async fn rejects_invalid_json_and_empty_frame() {
        let (mut writer, mut reader) = duplex(64);
        writer.write_all(&1_u32.to_be_bytes()).await.unwrap();
        writer.write_all(b"{").await.unwrap();

        assert!(matches!(
            read_frame::<_, RpcMessage>(&mut reader, 1024).await,
            Err(FrameError::Json(_))
        ));

        let (mut writer, mut reader) = duplex(64);
        writer.write_all(&0_u32.to_be_bytes()).await.unwrap();
        assert!(matches!(
            read_frame::<_, RpcMessage>(&mut reader, 1024).await,
            Err(FrameError::Empty)
        ));
    }
}
