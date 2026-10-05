use agent_providers::StdioProcess;
use futures_util::future::BoxFuture;
use std::io;
use tokio::io::{AsyncRead, AsyncWrite};

/// Lifetime control of a provider process owned by the Host's supervisor.
pub trait ProcessControl: Send {
    /// Resolves once the process has exited; true for a successful exit.
    fn wait(&mut self) -> BoxFuture<'_, bool>;
    fn kill(&mut self) -> BoxFuture<'_, ()>;
}

/// The stdio of one provider process. Frames are newline-delimited JSON.
pub struct ProviderProcess {
    pub input: Box<dyn AsyncWrite + Send + Unpin>,
    pub output: Box<dyn AsyncRead + Send + Unpin>,
    pub stderr: Box<dyn AsyncRead + Send + Unpin>,
    pub control: Box<dyn ProcessControl>,
}

impl ProviderProcess {
    /// Adopts a child spawned under the Host's supervisor; its stdio must be piped.
    pub fn from_child(child: tokio::process::Child) -> io::Result<Self> {
        let StdioProcess {
            child,
            input,
            output,
            stderr,
            ..
        } = StdioProcess::from_child(child)?;
        Ok(Self {
            input: Box::new(input),
            output: Box::new(output),
            stderr: Box::new(stderr),
            control: Box::new(ChildControl(child)),
        })
    }
}

struct ChildControl(tokio::process::Child);
impl ProcessControl for ChildControl {
    fn wait(&mut self) -> BoxFuture<'_, bool> {
        Box::pin(async move { self.0.wait().await.is_ok_and(|status| status.success()) })
    }
    fn kill(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let _ = self.0.kill().await;
        })
    }
}
