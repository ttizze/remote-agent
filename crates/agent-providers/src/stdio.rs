use serde_json::Value;
use std::{io, path::Path};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
};

/// `line` keeps a partially read frame when the caller cancels this future,
/// so the next call continues the same frame.
pub async fn read_frame(
    reader: &mut (impl AsyncBufRead + Unpin),
    line: &mut Vec<u8>,
) -> Result<Option<Value>, io::Error> {
    loop {
        if reader.read_until(b'\n', line).await? == 0 && line.is_empty() {
            return Ok(None);
        }
        let frame = std::mem::take(line);
        if frame.trim_ascii().is_empty() {
            continue;
        }
        return serde_json::from_slice(&frame)
            .map(Some)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
    }
}
pub async fn write_frame(
    writer: &mut (impl AsyncWrite + Unpin),
    frame: &Value,
) -> Result<(), io::Error> {
    let mut bytes = serde_json::to_vec(frame)?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await
}
/// The caller owns stderr draining and process lifetime. No capacity limit or
/// shared notification pump can make an unrelated provider session stop.
pub struct StdioProcess {
    pub child: Child,
    pub input: ChildStdin,
    pub output: BufReader<ChildStdout>,
    pub stderr: ChildStderr,
    line: Vec<u8>,
}
impl StdioProcess {
    pub fn spawn(
        executable: &Path,
        args: &[String],
        cwd: &Path,
        environment: &std::collections::BTreeMap<String, String>,
    ) -> io::Result<Self> {
        Self::from_child(
            Command::new(executable)
                .args(args)
                .current_dir(cwd)
                .env_clear()
                .envs(environment)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()?,
        )
    }
    /// Adopts a child spawned by another supervisor. Its stdin, stdout and
    /// stderr must be piped.
    pub fn from_child(mut child: Child) -> io::Result<Self> {
        Ok(Self {
            input: child
                .stdin
                .take()
                .ok_or_else(|| io::Error::other("missing provider stdin"))?,
            output: BufReader::new(
                child
                    .stdout
                    .take()
                    .ok_or_else(|| io::Error::other("missing provider stdout"))?,
            ),
            stderr: child
                .stderr
                .take()
                .ok_or_else(|| io::Error::other("missing provider stderr"))?,
            child,
            line: Vec::new(),
        })
    }
    pub async fn send(&mut self, frame: &Value) -> io::Result<()> {
        write_frame(&mut self.input, frame).await
    }
    pub async fn next_frame(&mut self) -> io::Result<Option<Value>> {
        read_frame(&mut self.output, &mut self.line).await
    }
    pub async fn close(mut self) -> io::Result<std::process::ExitStatus> {
        drop(self.input);
        let mut stdout_sink = tokio::io::sink();
        let mut stderr_sink = tokio::io::sink();
        let (status, stdout, stderr) = tokio::join!(
            self.child.wait(),
            tokio::io::copy(&mut self.output, &mut stdout_sink),
            tokio::io::copy(&mut self.stderr, &mut stderr_sink)
        );
        stdout?;
        stderr?;
        status
    }
}
