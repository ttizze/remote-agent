use std::io;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec, LinesCodecError};

/// Last-resort safety ceiling for one JSONL message.
///
/// This is a framing guard, not a product-level limit on Codex payloads.
/// The reader rejects an overlong line before it can grow without bound, and
/// the writer applies the same limit before putting a serialized message on
/// the wire.
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum JsonlError {
    #[error("JSONL I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("JSONL message exceeds maximum of {maximum} bytes")]
    MessageTooLarge { maximum: usize },
    #[error("JSONL message contains a literal line delimiter")]
    EmbeddedLineDelimiter,
}

/// Reads newline-delimited JSON values from any asynchronous byte stream.
///
/// The reader keeps each line as source text. Call classify_message separately
/// when a line needs routing metadata; this seam never decodes JSON fields.
/// A missing final newline is accepted by Tokio's line codec.
pub struct JsonlReader<R> {
    inner: FramedRead<R, LinesCodec>,
    maximum: usize,
}

impl<R> JsonlReader<R> {
    pub fn new(reader: R) -> Self {
        Self::with_max_message_bytes(reader, DEFAULT_MAX_MESSAGE_BYTES)
    }

    pub fn with_max_message_bytes(reader: R, maximum: usize) -> Self {
        Self {
            inner: FramedRead::new(reader, LinesCodec::new_with_max_length(maximum)),
            maximum,
        }
    }

    pub const fn max_message_bytes(&self) -> usize {
        self.maximum
    }

    pub fn into_inner(self) -> R {
        self.inner.into_inner()
    }
}

impl<R> JsonlReader<R>
where
    R: AsyncRead + Unpin,
{
    /// Reads one source JSON line, or None once the stream reaches EOF.
    pub async fn read_line(&mut self) -> Result<Option<String>, JsonlError> {
        let Some(line) = self.inner.next().await else {
            return Ok(None);
        };
        line.map(Some).map_err(|error| match error {
            LinesCodecError::MaxLineLengthExceeded => JsonlError::MessageTooLarge {
                maximum: self.maximum,
            },
            LinesCodecError::Io(error) => JsonlError::Io(error),
        })
    }

    /// Short alias for callers that treat the reader as a line stream.
    pub async fn read(&mut self) -> Result<Option<String>, JsonlError> {
        self.read_line().await
    }
}

/// Writes newline-delimited source JSON to any asynchronous byte stream.
pub struct JsonlWriter<W> {
    inner: FramedWrite<W, LinesCodec>,
    maximum: usize,
}

impl<W> JsonlWriter<W> {
    pub fn new(writer: W) -> Self {
        Self::with_max_message_bytes(writer, DEFAULT_MAX_MESSAGE_BYTES)
    }

    pub fn with_max_message_bytes(writer: W, maximum: usize) -> Self {
        Self {
            inner: FramedWrite::new(writer, LinesCodec::new_with_max_length(maximum)),
            maximum,
        }
    }

    pub const fn max_message_bytes(&self) -> usize {
        self.maximum
    }

    pub fn into_inner(self) -> W {
        self.inner.into_inner()
    }
}

impl<W> JsonlWriter<W>
where
    W: AsyncWrite + Unpin,
{
    /// Writes one source line followed by a newline.
    ///
    /// This transport seam intentionally does not parse or classify the line.
    /// Callers that need validation can pass it through classify_message.
    pub async fn write_line(&mut self, line: &str) -> Result<(), JsonlError> {
        if line
            .as_bytes()
            .iter()
            .any(|byte| matches!(byte, b'\n' | b'\r'))
        {
            return Err(JsonlError::EmbeddedLineDelimiter);
        }
        if line.len() > self.maximum {
            return Err(JsonlError::MessageTooLarge {
                maximum: self.maximum,
            });
        }
        self.send_line(line).await
    }

    async fn send_line(&mut self, line: &str) -> Result<(), JsonlError> {
        self.inner
            .send(line.to_owned())
            .await
            .map_err(|error| match error {
                LinesCodecError::MaxLineLengthExceeded => JsonlError::MessageTooLarge {
                    maximum: self.maximum,
                },
                LinesCodecError::Io(error) => JsonlError::Io(error),
            })
    }

    pub async fn flush(&mut self) -> Result<(), JsonlError> {
        SinkExt::<String>::flush(&mut self.inner)
            .await
            .map_err(|error| match error {
                LinesCodecError::MaxLineLengthExceeded => JsonlError::MessageTooLarge {
                    maximum: self.maximum,
                },
                LinesCodecError::Io(error) => JsonlError::Io(error),
            })
    }

    pub async fn shutdown(&mut self) -> Result<(), JsonlError> {
        SinkExt::<String>::close(&mut self.inner)
            .await
            .map_err(|error| match error {
                LinesCodecError::MaxLineLengthExceeded => JsonlError::MessageTooLarge {
                    maximum: self.maximum,
                },
                LinesCodecError::Io(error) => JsonlError::Io(error),
            })
    }

    /// Short alias for callers that treat the writer as a line sink.
    pub async fn write(&mut self, line: &str) -> Result<(), JsonlError> {
        self.write_line(line).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncWriteExt, duplex};

    #[tokio::test]
    async fn round_trips_source_lines_without_decoding_them() {
        let request = r#"{"id":"turn-1","method":"turn/start","params":{"text":"hello"}}"#;
        let notification = r#"{"method":"turn/completed","params":{"future":{"id":7}}}"#;
        let response = r#"{"id":"turn-1","result":{"ok":true},"unknown":[1,2,3]}"#;
        let (client, server) = duplex(16 * 1024);
        let (client_read, client_write) = tokio::io::split(client);
        let (server_read, server_write) = tokio::io::split(server);

        let mut writer = JsonlWriter::new(client_write);
        writer.write_line(request).await.unwrap();
        writer.write_line(notification).await.unwrap();
        writer.write_line(response).await.unwrap();
        writer.shutdown().await.unwrap();

        let mut reader = JsonlReader::new(server_read);
        assert_eq!(reader.read_line().await.unwrap().as_deref(), Some(request));
        assert_eq!(
            reader.read_line().await.unwrap().as_deref(),
            Some(notification)
        );
        assert_eq!(reader.read_line().await.unwrap().as_deref(), Some(response));
        drop(server_write);
        assert!(reader.read_line().await.unwrap().is_none());
        drop(client_read);
    }

    #[tokio::test]
    async fn rejects_an_overlong_line_before_classification() {
        let (mut writer, reader) = duplex(1024);
        writer
            .write_all(b"{\"method\":\"this line is too long\"}\n")
            .await
            .unwrap();
        let mut reader = JsonlReader::with_max_message_bytes(reader, 8);

        assert!(matches!(
            reader.read_line().await,
            Err(JsonlError::MessageTooLarge { maximum: 8 })
        ));
    }

    #[tokio::test]
    async fn applies_the_same_bound_to_outbound_lines() {
        let (writer, _reader) = duplex(1024);
        let mut writer = JsonlWriter::with_max_message_bytes(writer, 8);

        let error = writer.write_line(r#"{"too":"large"}"#).await.unwrap_err();
        assert!(matches!(error, JsonlError::MessageTooLarge { maximum: 8 }));
    }

    #[tokio::test]
    async fn rejects_embedded_line_delimiters() {
        let (writer, _reader) = duplex(1024);
        let mut writer = JsonlWriter::new(writer);

        assert!(matches!(
            writer.write_line("{}\n{}").await,
            Err(JsonlError::EmbeddedLineDelimiter)
        ));
    }

    #[tokio::test]
    async fn accepts_a_final_line_without_a_newline() {
        let (mut writer, reader) = duplex(1024);
        writer
            .write_all(br#"{"method":"done","params":{}}"#)
            .await
            .unwrap();
        writer.shutdown().await.unwrap();
        let mut reader = JsonlReader::new(reader);

        assert_eq!(
            reader.read_line().await.unwrap().as_deref(),
            Some(r#"{"method":"done","params":{}}"#)
        );
        assert!(reader.read_line().await.unwrap().is_none());
    }
}
