use std::io;

use futures_util::StreamExt;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufWriter};
use tokio_util::codec::{FramedRead, LinesCodec, LinesCodecError};

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
/// The reader keeps each line as source text. Call RpcMessage::parse separately
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
}

/// Writes newline-delimited source JSON to any asynchronous byte stream.
pub struct JsonlWriter<W: AsyncWrite> {
    inner: BufWriter<W>,
    maximum: usize,
}

impl<W: AsyncWrite> JsonlWriter<W> {
    pub fn new(writer: W) -> Self {
        Self::with_max_message_bytes(writer, DEFAULT_MAX_MESSAGE_BYTES)
    }

    pub fn with_max_message_bytes(writer: W, maximum: usize) -> Self {
        Self {
            inner: BufWriter::new(writer),
            maximum,
        }
    }

    pub const fn max_message_bytes(&self) -> usize {
        self.maximum
    }
}

impl<W> JsonlWriter<W>
where
    W: AsyncWrite + Unpin,
{
    /// Writes one source line followed by a newline.
    ///
    /// This transport seam intentionally does not parse or classify the line.
    /// Callers that need validation can pass it through RpcMessage::parse.
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
        self.inner.write_all(line.as_bytes()).await?;
        self.inner.write_all(b"\n").await?;
        self.flush().await
    }

    async fn flush(&mut self) -> Result<(), JsonlError> {
        self.inner.flush().await.map_err(Into::into)
    }

    pub async fn shutdown(&mut self) -> Result<(), JsonlError> {
        self.inner.shutdown().await.map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncWriteExt, duplex};

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
}
