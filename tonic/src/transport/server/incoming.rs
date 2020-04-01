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
                    let io = tls.accept(stream);
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

// tokio_rustls::server::TlsStream doesn't expose constructor methods,
// so we have to TlsAcceptor::accept and handshake to have access to it
// TlsStream implements AsyncRead/AsyncWrite handshaking tokio_rustls::Accept first
#[cfg(feature = "tls")]
pub(crate) struct TlsStream<IO> {
    state: State<IO>,
}

#[cfg(feature = "tls")]
enum State<IO> {
    Handshaking(tokio_rustls::Accept<IO>),
    Streaming(tokio_rustls::server::TlsStream<IO>),
}

#[cfg(feature = "tls")]
impl<IO> TlsStream<IO> {
    pub(crate) fn new(accept: tokio_rustls::Accept<IO>) -> Self {
        TlsStream {
            state: State::Handshaking(accept),
        }
    }

    pub(crate) fn get_ref(&self) -> Option<(&IO, &tokio_rustls::rustls::ServerSession)> {
        if let State::Streaming(tls) = &self.state {
            Some(tls.get_ref())
        } else {
            None
        }
    }
}

#[cfg(feature = "tls")]
impl<IO> AsyncRead for TlsStream<IO>
where
    IO: AsyncRead + AsyncWrite + Connected + Unpin + Send + 'static,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        use std::future::Future;

        let pin = self.get_mut();
        match pin.state {
            State::Handshaking(ref mut accept) => {
                match futures_core::ready!(Pin::new(accept).poll(cx)) {
                    Ok(mut stream) => {
                        let result = Pin::new(&mut stream).poll_read(cx, buf);
                        pin.state = State::Streaming(stream);
                        result
                    }
                    Err(err) => Poll::Ready(Err(err)),
                }
            }
            State::Streaming(ref mut stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

#[cfg(feature = "tls")]
impl<IO> AsyncWrite for TlsStream<IO>
where
    IO: AsyncRead + AsyncWrite + Connected + Unpin + Send + 'static,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        use std::future::Future;

        let pin = self.get_mut();
        match pin.state {
            State::Handshaking(ref mut accept) => {
                match futures_core::ready!(Pin::new(accept).poll(cx)) {
                    Ok(mut stream) => {
                        let result = Pin::new(&mut stream).poll_write(cx, buf);
                        pin.state = State::Streaming(stream);
                        result
                    }
                    Err(err) => Poll::Ready(Err(err)),
                }
            }
            State::Streaming(ref mut stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.state {
            State::Handshaking(_) => Poll::Ready(Ok(())),
            State::Streaming(ref mut stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.state {
            State::Handshaking(_) => Poll::Ready(Ok(())),
            State::Streaming(ref mut stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}
