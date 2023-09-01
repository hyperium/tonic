use super::{Connected, Server};
use crate::transport::service::ServerIo;
use hyper::server::{
    accept::Accept,
    conn::{AddrIncoming, AddrStream},
};
use std::{
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpListener,
};
use tokio_stream::{Stream, StreamExt};

#[cfg(not(feature = "tls"))]
pub(crate) fn tcp_incoming<IO, IE, L>(
    incoming: impl Stream<Item = Result<IO, IE>>,
    _server: Server<L>,
) -> impl Stream<Item = Result<ServerIo<IO>, crate::Error>>
where
    IO: AsyncRead + AsyncWrite + Connected + Unpin + Send + 'static,
    IE: Into<crate::Error>,
{
    async_stream::try_stream! {
        tokio::pin!(incoming);

        while let Some(item) = incoming.next().await {
            yield item.map(ServerIo::new_io)?
        }
    }
}

#[cfg(feature = "tls")]
pub(crate) fn tcp_incoming<IO, IE, L>(
    incoming: impl Stream<Item = Result<IO, IE>>,
    server: Server<L>,
) -> impl Stream<Item = Result<ServerIo<IO>, crate::Error>>
where
    IO: AsyncRead + AsyncWrite + Connected + Unpin + Send + 'static,
    IE: Into<crate::Error>,
{
    async_stream::try_stream! {
        tokio::pin!(incoming);

        let mut tasks = tokio::task::JoinSet::new();

        loop {
            match select(&mut incoming, &mut tasks).await {
                SelectOutput::Incoming(stream) => {
                    if let Some(tls) = &server.tls {
                        let tls = tls.clone();
                        tasks.spawn(async move {
                            let io = tls.accept(stream).await?;
                            Ok(ServerIo::new_tls_io(io))
                        });
                    } else {
                        yield ServerIo::new_io(stream);
                    }
                }

                SelectOutput::Io(io) => {
                    yield io;
                }

                SelectOutput::Err(e) => {
                    tracing::debug!(message = "Accept loop error.", error = %e);
                }

                SelectOutput::Done => {
                    break;
                }
            }
        }
    }
}

#[cfg(feature = "tls")]
async fn select<IO: 'static, IE>(
    incoming: &mut (impl Stream<Item = Result<IO, IE>> + Unpin),
    tasks: &mut tokio::task::JoinSet<Result<ServerIo<IO>, crate::Error>>,
) -> SelectOutput<IO>
where
    IE: Into<crate::Error>,
{
    if tasks.is_empty() {
        return match incoming.try_next().await {
            Ok(Some(stream)) => SelectOutput::Incoming(stream),
            Ok(None) => SelectOutput::Done,
            Err(e) => SelectOutput::Err(e.into()),
        };
    }

    tokio::select! {
        stream = incoming.try_next() => {
            match stream {
                Ok(Some(stream)) => SelectOutput::Incoming(stream),
                Ok(None) => SelectOutput::Done,
                Err(e) => SelectOutput::Err(e.into()),
            }
        }

        accept = tasks.join_next() => {
            match accept.expect("JoinSet should never end") {
                Ok(Ok(io)) => SelectOutput::Io(io),
                Ok(Err(e)) => SelectOutput::Err(e),
                Err(e) => SelectOutput::Err(e.into()),
            }
        }
    }
}

#[cfg(feature = "tls")]
enum SelectOutput<A> {
    Incoming(A),
    Io(ServerIo<A>),
    Err(crate::Error),
    Done,
}

/// Binds a socket address for a [Router](super::Router)
///
/// An incoming stream, usable with [Router::serve_with_incoming](super::Router::serve_with_incoming),
/// of `AsyncRead + AsyncWrite` that communicate with clients that connect to a socket address.
#[derive(Debug)]
pub struct TcpIncoming {
    inner: AddrIncoming,
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
    /// # use tonic::{body::BoxBody, server::NamedService, transport::{Body, Server, server::TcpIncoming}};
    /// # use core::convert::Infallible;
    /// # use std::error::Error;
    /// # fn main() { }  // Cannot have type parameters, hence instead define:
    /// # fn run<S>(some_service: S) -> Result<(), Box<dyn Error + Send + Sync>>
    /// # where
    /// #   S: Service<Request<Body>, Response = Response<BoxBody>, Error = Infallible> + NamedService + Clone + Send + 'static,
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
    ) -> Result<Self, crate::Error> {
        let mut inner = AddrIncoming::bind(&addr)?;
        inner.set_nodelay(nodelay);
        inner.set_keepalive(keepalive);
        Ok(TcpIncoming { inner })
    }

    /// Creates a new `TcpIncoming` from an existing `tokio::net::TcpListener`.
    pub fn from_listener(
        listener: TcpListener,
        nodelay: bool,
        keepalive: Option<Duration>,
    ) -> Result<Self, crate::Error> {
        let mut inner = AddrIncoming::from_listener(listener)?;
        inner.set_nodelay(nodelay);
        inner.set_keepalive(keepalive);
        Ok(TcpIncoming { inner })
    }
}

impl Stream for TcpIncoming {
    type Item = Result<AddrStream, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_accept(cx)
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
