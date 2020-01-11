use crate::transport::{server::Connected, Certificate};
use hyper::client::connect::{Connected as HyperConnected, Connection};
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite};

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

impl Connected for BoxedIo {}

impl AsyncRead for BoxedIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
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
}

pub(in crate::transport) trait ConnectedIo: Io + Connected {}

impl<T> ConnectedIo for T where T: Io + Connected {}

pub(crate) struct ServerIo(Pin<Box<dyn ConnectedIo>>);

impl ServerIo {
    pub(in crate::transport) fn new<I: ConnectedIo>(io: I) -> Self {
        ServerIo(Box::pin(io))
    }
}

impl Connected for ServerIo {
    fn remote_addr(&self) -> Option<SocketAddr> {
        (&*self.0).remote_addr()
    }

    fn peer_certs(&self) -> Option<Vec<Certificate>> {
        (&self.0).peer_certs()
    }
}

impl AsyncRead for ServerIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for ServerIo {
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
