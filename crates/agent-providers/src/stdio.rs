use serde_json::Value;
use std::{io, path::Path};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
};

pub async fn read_frame(
    reader: &mut (impl AsyncBufRead + Unpin),
) -> Result<Option<Value>, io::Error> {
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            return Ok(None);
        }
        if line.trim().is_empty() {
            continue;
        }
        return serde_json::from_str(&line)
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
}
impl StdioProcess {
    pub fn spawn(executable: &Path, args: &[String], cwd: &Path) -> io::Result<Self> {
        let mut child = Command::new(executable)
            .args(args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
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
        })
    }
    pub async fn send(&mut self, frame: &Value) -> io::Result<()> {
        write_frame(&mut self.input, frame).await
    }
    pub async fn next_frame(&mut self) -> io::Result<Option<Value>> {
        read_frame(&mut self.output).await
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
