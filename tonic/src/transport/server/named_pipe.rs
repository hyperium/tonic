use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio_stream::Stream;

/// An incoming stream of Windows named pipe connections.
///
/// Use this with `Server::serve_with_incoming`.
pub struct NamedPipeIncoming {
    pipe_name: String,
    connecting: Option<Pin<Box<dyn Future<Output = io::Result<NamedPipeServer>> + Send>>>,
}

impl NamedPipeIncoming {
    /// Create a new named pipe incoming stream.
    ///
    /// The `pipe_name` may be a full pipe path like `\\.\pipe\my-pipe` or a
    /// short name like `my-pipe`.
    pub fn new(pipe_name: impl AsRef<str>) -> Self {
        Self {
            pipe_name: normalize_pipe_path(pipe_name.as_ref()),
            connecting: None,
        }
    }
}

impl Stream for NamedPipeIncoming {
    type Item = io::Result<NamedPipeServer>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.connecting.is_none() {
            let pipe_name = self.pipe_name.clone();
            let fut = async move {
                let server = ServerOptions::new().create(pipe_name)?;
                server.connect().await?;
                Ok(server)
            };
            self.connecting = Some(Box::pin(fut));
        }

        let ready = {
            let fut = self.connecting.as_mut().expect("future just initialized");
            fut.as_mut().poll(cx)
        };

        match ready {
            Poll::Ready(result) => {
                self.connecting = None;
                Poll::Ready(Some(result))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

fn normalize_pipe_path(raw: &str) -> String {
    if raw.starts_with(r"\\.\pipe\") {
        return raw.to_string();
    }

    let mut s = raw.trim().trim_start_matches('/');
    if let Some(stripped) = s.strip_prefix(r"\\.\pipe\") {
        s = stripped;
    }
    if let Some(stripped) = s.strip_prefix("./") {
        s = stripped;
    }
    if let Some(stripped) = s.strip_prefix("pipe/") {
        s = stripped;
    }
    if let Some(stripped) = s.strip_prefix("/pipe/") {
        s = stripped;
    }
    let s = s.trim_start_matches('/');
    let mut path = String::from(r"\\.\pipe\");
    path.push_str(&s.replace('/', "\\"));
    path
}
