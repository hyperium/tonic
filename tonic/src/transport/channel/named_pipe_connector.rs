use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use http::Uri;
use hyper_util::rt::TokioIo;
use tower::Service;

use crate::status::ConnectError;

#[cfg(windows)]
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};

#[cfg(windows)]
async fn connect_named_pipe(pipe_path: String) -> Result<NamedPipeClient, ConnectError> {
    ClientOptions::new()
        .open(pipe_path)
        .map_err(|err| ConnectError(From::from(err)))
}

// Dummy type that will allow us to compile and match trait bounds
// but is never used.
#[cfg(not(windows))]
#[allow(dead_code)]
type NamedPipeClient = tokio::io::DuplexStream;

#[cfg(not(windows))]
async fn connect_named_pipe(_pipe_path: String) -> Result<NamedPipeClient, ConnectError> {
    Err(ConnectError(
        "named pipe connections are only supported on windows".into(),
    ))
}

pub(crate) struct NamedPipeConnector {
    pipe_path: String,
}

impl NamedPipeConnector {
    pub(crate) fn new(pipe_path: &str) -> Self {
        NamedPipeConnector {
            pipe_path: pipe_path.to_string(),
        }
    }
}

impl Service<Uri> for NamedPipeConnector {
    type Response = TokioIo<NamedPipeClient>;
    type Error = ConnectError;
    type Future = NamedPipeConnecting;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: Uri) -> Self::Future {
        let pipe_path = self.pipe_path.clone();
        let fut = async move {
            let stream = connect_named_pipe(pipe_path).await?;
            Ok(TokioIo::new(stream))
        };
        NamedPipeConnecting {
            inner: Box::pin(fut),
        }
    }
}

type ConnectResult = Result<TokioIo<NamedPipeClient>, ConnectError>;

pub(crate) struct NamedPipeConnecting {
    inner: Pin<Box<dyn Future<Output = ConnectResult> + Send>>,
}

impl Future for NamedPipeConnecting {
    type Output = ConnectResult;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().inner.as_mut().poll(cx)
    }
}
