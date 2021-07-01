use super::{Connected, Server};
use crate::transport::service::ServerIo;
use futures_core::Stream;
use futures_util::stream::TryStreamExt;
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
use tokio::io::{AsyncRead, AsyncWrite};

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
        futures_util::pin_mut!(incoming);

        while let Some(stream) = incoming.try_next().await? {
            yield ServerIo::new_io(stream);
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
        futures_util::pin_mut!(incoming);

        #[cfg(feature = "tls")]
        let mut tasks = futures_util::stream::futures_unordered::FuturesUnordered::new();

        loop {
            match select(&mut incoming, &mut tasks).await {
                SelectOutput::Incoming(stream) => {
                    if let Some(tls) = &server.tls {
                        let tls = tls.clone();

                        let accept = tokio::spawn(async move {
                            let io = tls.accept(stream).await?;
                            Ok(ServerIo::new_tls_io(io))
                        });

                        tasks.push(accept);
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
async fn select<IO, IE>(
    incoming: &mut (impl Stream<Item = Result<IO, IE>> + Unpin),
    tasks: &mut futures_util::stream::futures_unordered::FuturesUnordered<
        tokio::task::JoinHandle<Result<ServerIo<IO>, crate::Error>>,
    >,
) -> SelectOutput<IO>
where
    IE: Into<crate::Error>,
{
    use futures_util::StreamExt;

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

        accept = tasks.next() => {
            match accept.expect("FuturesUnordered stream should never end") {
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

pub(crate) struct TcpIncoming {
    inner: AddrIncoming,
}

impl TcpIncoming {
    pub(crate) fn new(
        addr: SocketAddr,
        nodelay: bool,
        keepalive: Option<Duration>,
    ) -> Result<Self, crate::Error> {
        let mut inner = AddrIncoming::bind(&addr)?;
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
