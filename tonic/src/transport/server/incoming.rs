use std::{
    net::{SocketAddr, TcpListener as StdTcpListener},
    pin::Pin,
    task::{ready, Context, Poll},
    time::Duration,
};

use tokio::net::{TcpListener, TcpStream};
use tokio_stream::{wrappers::TcpListenerStream, Stream};
use tracing::warn;

/// Binds a socket address for a [Router](super::Router)
///
/// An incoming stream, usable with [Router::serve_with_incoming](super::Router::serve_with_incoming),
/// of `AsyncRead + AsyncWrite` that communicate with clients that connect to a socket address.
#[derive(Debug)]
pub struct TcpIncoming {
    inner: TcpListenerStream,
    nodelay: bool,
    keepalive: Option<Duration>,
}

impl TcpIncoming {
    /// Creates an instance by binding (opening) the specified socket address
    /// to which the specified TCP 'nodelay' and 'keepalive' parameters are applied.
    /// Returns a TcpIncoming if the socket address was successfully bound.
    ///
    /// # Examples
    /// ```no_run
    /// # use tower_service::Service;
    /// # use http::{request::Request, response::Response};
    /// # use tonic::{body::Body, server::NamedService, transport::{Server, server::TcpIncoming}};
    /// # use core::convert::Infallible;
    /// # use std::error::Error;
    /// # fn main() { }  // Cannot have type parameters, hence instead define:
    /// # fn run<S>(some_service: S) -> Result<(), Box<dyn Error + Send + Sync>>
    /// # where
    /// #   S: Service<Request<Body>, Response = Response<Body>, Error = Infallible> + NamedService + Clone + Send + 'static,
    /// #   S::Future: Send + 'static,
    /// # {
    /// // Find a free port
    /// let mut port = 1322;
    /// let tinc = loop {
    ///    let addr = format!("127.0.0.1:{}", port).parse().unwrap();
    ///    match TcpIncoming::new(addr, true, None) {
    ///       Ok(t) => break t,
    ///       Err(_) => port += 1
    ///    }
    /// };
    /// Server::builder()
    ///    .add_service(some_service)
    ///    .serve_with_incoming(tinc);
    /// # Ok(())
    /// # }
    pub fn new(
        addr: SocketAddr,
        nodelay: bool,
        keepalive: Option<Duration>,
    ) -> Result<Self, crate::BoxError> {
        let std_listener = StdTcpListener::bind(addr)?;
        std_listener.set_nonblocking(true)?;

        let inner = TcpListenerStream::new(TcpListener::from_std(std_listener)?);
        Ok(Self {
            inner,
            nodelay,
            keepalive,
        })
    }

    /// Creates a new `TcpIncoming` from an existing `tokio::net::TcpListener`.
    pub fn from_listener(
        listener: TcpListener,
        nodelay: bool,
        keepalive: Option<Duration>,
    ) -> Result<Self, crate::BoxError> {
        Ok(Self {
            inner: TcpListenerStream::new(listener),
            nodelay,
            keepalive,
        })
    }
}

impl Stream for TcpIncoming {
    type Item = Result<TcpStream, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match ready!(Pin::new(&mut self.inner).poll_next(cx)) {
            Some(Ok(stream)) => {
                set_accepted_socket_options(&stream, self.nodelay, self.keepalive);
                Some(Ok(stream)).into()
            }
            other => Poll::Ready(other),
        }
    }
}

// Consistent with hyper-0.14, this function does not return an error.
fn set_accepted_socket_options(stream: &TcpStream, nodelay: bool, keepalive: Option<Duration>) {
    if nodelay {
        if let Err(e) = stream.set_nodelay(true) {
            warn!("error trying to set TCP nodelay: {}", e);
        }
    }

    if let Some(timeout) = keepalive {
        let sock_ref = socket2::SockRef::from(&stream);
        let sock_keepalive = socket2::TcpKeepalive::new().with_time(timeout);

        if let Err(e) = sock_ref.set_tcp_keepalive(&sock_keepalive) {
            warn!("error trying to set TCP keepalive: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::transport::server::TcpIncoming;
    #[tokio::test]
    async fn one_tcpincoming_at_a_time() {
        let addr = "127.0.0.1:1322".parse().unwrap();
        {
            let _t1 = TcpIncoming::new(addr, true, None).unwrap();
            let _t2 = TcpIncoming::new(addr, true, None).unwrap_err();
        }
        let _t3 = TcpIncoming::new(addr, true, None).unwrap();
    }
}
