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
#[cfg(feature = "tls")]
use tracing::error;

#[cfg_attr(not(feature = "tls"), allow(unused_variables))]
pub(crate) fn tcp_incoming<IO, IE>(
    incoming: impl Stream<Item = Result<IO, IE>>,
    server: Server,
) -> impl Stream<Item = Result<ServerIo, crate::Error>>
where
    IO: AsyncRead + AsyncWrite + Connected + Unpin + Send + 'static,
    IE: Into<crate::Error>,
{
    async_stream::try_stream! {
        futures_util::pin_mut!(incoming);

        while let Some(stream) = incoming.try_next().await? {
            #[cfg(feature = "tls")]
            {
                if let Some(tls) = &server.tls {
                    let io = match tls.accept(stream).await {
                        Ok(io) => io,
                        Err(error) => {
                            error!(message = "Unable to accept incoming connection.", %error);
                            continue
                        },
                    };
                    yield ServerIo::new(io);
                    continue;
                }
            }

            yield ServerIo::new(stream);
        }
    }
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
