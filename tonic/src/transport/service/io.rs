use std::fmt::{Debug, Error, Formatter};
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite};

pub(in crate::transport) trait Io:
AsyncRead + AsyncWrite + Send + Unpin + 'static
{}

impl<T> Io for T where T: AsyncRead + AsyncWrite + Send + Unpin + 'static {}

// IO structure used by GRPC servers
pub(crate) struct ServerIo {
    io: Pin<Box<dyn Io>>,
    #[cfg(feature = "tls_client_identity")]
    pub(crate) client_identity: Option<String>,
}

impl ServerIo {
    #[cfg(not(feature = "tls_client_identity"))]
    pub(in crate::transport) fn new<I: Io>(io: I) -> Self {
        ServerIo {
            io: Box::pin(io),
        }
    }

    #[cfg(feature = "tls_client_identity")]
    pub(in crate::transport) fn new<I: Io>(io: I, client_identity: Option<String>) -> Self {
        ServerIo {
            io: Box::pin(io),
            client_identity,
        }
    }
}

impl Debug for ServerIo {
    #[cfg(feature = "tls_client_identity")]
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), Error> {
        f.write_fmt(format_args!(
            "ServerIo(Identity: {:?})",
            self.client_identity
        ))
    }

    #[cfg(not(feature = "tls_client_identity"))]
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), Error> {
        f.write_str("ServerIo")
    }
}

impl AsyncRead for ServerIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl AsyncWrite for ServerIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

/// IO structure used by GRPC clients
pub(crate) struct ClientIo(Pin<Box<dyn Io>>);

impl ClientIo {
    pub(in crate::transport) fn new<I: Io>(io: I) -> Self {
        ClientIo(Box::pin(io))
    }
}

impl Debug for ClientIo {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), Error> {
        f.write_str("ClientIo")
    }
}

impl AsyncRead for ClientIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for ClientIo {
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
}
