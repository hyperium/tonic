use hyper::server::conn::AddrStream;
use std::net::SocketAddr;
#[cfg(feature = "tls")]
use tokio_rustls::TlsStream;

/// Trait that connected IO resources implement.
///
/// The goal for this trait is to allow users to implement
/// custom IO types that can still provide the same connection
/// metadata.
pub trait Connected {
    /// Return the remote address this IO resource is connected too.
    fn remote_addr(&self) -> Option<SocketAddr> {
        None
    }
}

impl Connected for AddrStream {
    fn remote_addr(&self) -> Option<SocketAddr> {
        Some(self.remote_addr())
    }
}

#[cfg(feature = "tls")]
impl<T: Connected> Connected for TlsStream<T> {
    fn remote_addr(&self) -> Option<SocketAddr> {
        let (inner, _) = self.get_ref();
        inner.remote_addr()
    }
}
