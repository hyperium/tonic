use crate::transport::server::Connected;
use hyper::client::connect::{Connected as HyperConnected, Connection};
use std::io;
use std::io::IoSlice;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
#[cfg(feature = "tls")]
use tokio_rustls::server::TlsStream;

pub(in crate::transport) trait Io:
    AsyncRead + AsyncWrite + Send + 'static
{
}

impl<T> Io for T where T: AsyncRead + AsyncWrite + Send + 'static {}

pub(crate) struct BoxedIo(Pin<Box<dyn Io>>);

impl BoxedIo {
    pub(in crate::transport) fn new<I: Io>(io: I) -> Self {
        BoxedIo(Box::pin(io))
    }
}

impl Connection for BoxedIo {
    fn connected(&self) -> HyperConnected {
        HyperConnected::new()
    }
}

impl Connected for BoxedIo {
    type ConnectInfo = NoneConnectInfo;

    fn connect_info(&self) -> Self::ConnectInfo {
        NoneConnectInfo
    }
}

#[derive(Copy, Clone)]
pub(crate) struct NoneConnectInfo;

impl AsyncRead for BoxedIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for BoxedIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<Result<usize, io::Error>> {
        Pin::new(&mut self.0).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.0.is_write_vectored()
    }
}

pub(crate) enum ServerIo<IO> {
    Io(IO),
    #[cfg(feature = "tls")]
    TlsIo(Box<TlsStream<IO>>),
}

use tower::util::Either;

#[cfg(feature = "tls")]
type ServerIoConnectInfo<IO> =
    Either<<IO as Connected>::ConnectInfo, <TlsStream<IO> as Connected>::ConnectInfo>;

#[cfg(not(feature = "tls"))]
type ServerIoConnectInfo<IO> = Either<<IO as Connected>::ConnectInfo, ()>;

impl<IO> ServerIo<IO> {
    pub(in crate::transport) fn new_io(io: IO) -> Self {
        Self::Io(io)
    }

    #[cfg(feature = "tls")]
    pub(in crate::transport) fn new_tls_io(io: TlsStream<IO>) -> Self {
        Self::TlsIo(Box::new(io))
    }

    #[cfg(feature = "tls")]
    pub(in crate::transport) fn connect_info(&self) -> ServerIoConnectInfo<IO>
    where
        IO: Connected,
        TlsStream<IO>: Connected,
    {
        match self {
            Self::Io(io) => Either::A(io.connect_info()),
            Self::TlsIo(io) => Either::B(io.connect_info()),
        }
    }

    #[cfg(not(feature = "tls"))]
    pub(in crate::transport) fn connect_info(&self) -> ServerIoConnectInfo<IO>
    where
        IO: Connected,
    {
        match self {
            Self::Io(io) => Either::A(io.connect_info()),
        }
    }
}

impl<IO> AsyncRead for ServerIo<IO>
where
    IO: AsyncWrite + AsyncRead + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match &mut *self {
            Self::Io(io) => Pin::new(io).poll_read(cx, buf),
            #[cfg(feature = "tls")]
            Self::TlsIo(io) => Pin::new(io).poll_read(cx, buf),
        }
    }
}

impl<IO> AsyncWrite for ServerIo<IO>
where
    IO: AsyncWrite + AsyncRead + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut *self {
            Self::Io(io) => Pin::new(io).poll_write(cx, buf),
            #[cfg(feature = "tls")]
            Self::TlsIo(io) => Pin::new(io).poll_write(cx, buf),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            Self::Io(io) => Pin::new(io).poll_flush(cx),
            #[cfg(feature = "tls")]
            Self::TlsIo(io) => Pin::new(io).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            Self::Io(io) => Pin::new(io).poll_shutdown(cx),
            #[cfg(feature = "tls")]
            Self::TlsIo(io) => Pin::new(io).poll_shutdown(cx),
        }
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<Result<usize, io::Error>> {
        match &mut *self {
            Self::Io(io) => Pin::new(io).poll_write_vectored(cx, bufs),
            #[cfg(feature = "tls")]
            Self::TlsIo(io) => Pin::new(io).poll_write_vectored(cx, bufs),
        }
    }

    fn is_write_vectored(&self) -> bool {
        match self {
            Self::Io(io) => io.is_write_vectored(),
            #[cfg(feature = "tls")]
            Self::TlsIo(io) => io.is_write_vectored(),
        }
    }
}
