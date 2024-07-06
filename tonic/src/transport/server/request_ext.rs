use std::net::SocketAddr;
#[cfg(feature = "tls")]
use std::sync::Arc;

#[cfg(feature = "tls")]
use tokio_rustls::rustls::pki_types::CertificateDer;

use super::TcpConnectInfo;
#[cfg(feature = "tls")]
use super::TlsConnectInfo;
use crate::Request;

mod sealed {
    pub trait Sealed {}
}

impl<T> sealed::Sealed for Request<T> {}

/// An extension trait adding utility methods to [`Request`].
pub trait RequestExt: sealed::Sealed {
    /// Get the local address of this connection.
    ///
    /// This will return `None` if the `IO` type used
    /// does not implement `Connected` or when using a unix domain socket.
    /// This currently only works on the server side.
    fn local_addr(&self) -> Option<SocketAddr>;

    /// Get the remote address of this connection.
    ///
    /// This will return `None` if the `IO` type used
    /// does not implement `Connected` or when using a unix domain socket.
    /// This currently only works on the server side.
    fn remote_addr(&self) -> Option<SocketAddr>;

    /// Get the peer certificates of the connected client.
    ///
    /// This is used to fetch the certificates from the TLS session
    /// and is mostly used for mTLS. This currently only returns
    /// `Some` on the server side of the `transport` server with
    /// TLS enabled connections.
    #[cfg(feature = "tls")]
    fn peer_certs(&self) -> Option<Arc<Vec<CertificateDer<'static>>>>;
}

impl<T> RequestExt for Request<T> {
    fn local_addr(&self) -> Option<SocketAddr> {
        let addr = self
            .extensions()
            .get::<TcpConnectInfo>()
            .and_then(|i| i.local_addr());

        #[cfg(feature = "tls")]
        let addr = addr.or_else(|| {
            self.extensions()
                .get::<TlsConnectInfo<TcpConnectInfo>>()
                .and_then(|i| i.get_ref().local_addr())
        });

        addr
    }

    fn remote_addr(&self) -> Option<SocketAddr> {
        let addr = self
            .extensions()
            .get::<TcpConnectInfo>()
            .and_then(|i| i.remote_addr());

        #[cfg(feature = "tls")]
        let addr = addr.or_else(|| {
            self.extensions()
                .get::<TlsConnectInfo<TcpConnectInfo>>()
                .and_then(|i| i.get_ref().remote_addr())
        });

        addr
    }

    #[cfg(feature = "tls")]
    fn peer_certs(&self) -> Option<Arc<Vec<CertificateDer<'static>>>> {
        self.extensions()
            .get::<TlsConnectInfo<TcpConnectInfo>>()
            .and_then(|i| i.peer_certs())
    }
}
