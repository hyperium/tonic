/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

use std::{
    net::{SocketAddr, TcpListener as StdTcpListener},
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use socket2::TcpKeepalive;
use tokio::net::{TcpListener, TcpStream};
use tokio_stream::{Stream, wrappers::TcpListenerStream};
use tracing::warn;

/// Binds a socket address for a [Router](super::Router)
///
/// An incoming stream, usable with [Router::serve_with_incoming](super::Router::serve_with_incoming),
/// of `AsyncRead + AsyncWrite` that communicate with clients that connect to a socket address.
#[derive(Debug)]
pub struct TcpIncoming {
    inner: TcpListenerStream,
    nodelay: Option<bool>,
    keepalive: Option<TcpKeepalive>,
    keepalive_time: Option<Duration>,
    keepalive_interval: Option<Duration>,
    keepalive_retries: Option<u32>,
}

impl TcpIncoming {
    /// Creates an instance by binding (opening) the specified socket address.
    ///
    /// Returns a TcpIncoming if the socket address was successfully bound.
    ///
    /// If the process was launched under a socket-activation manager
    /// that passed a listening socket matching `addr` via the
    /// `LISTEN_FDS` / `LISTEN_PID` environment variables, that inherited
    /// descriptor is adopted instead of opening a new socket. This behavior
    /// requires the `socket-activation` feature (Unix only).
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
    /// #   S: Service<Request<Body>, Response = Response<Body>, Error = Infallible> + NamedService + Clone + Send + Sync + 'static,
    /// #   S::Future: Send + 'static,
    /// # {
    /// // Find a free port
    /// let mut port = 1322;
    /// let tinc = loop {
    ///    let addr = format!("127.0.0.1:{}", port).parse().unwrap();
    ///    match TcpIncoming::bind(addr) {
    ///       Ok(t) => break t,
    ///       Err(_) => port += 1
    ///    }
    /// };
    /// Server::builder()
    ///    .add_service(some_service)
    ///    .serve_with_incoming(tinc);
    /// # Ok(())
    /// # }
    pub fn bind(addr: SocketAddr) -> std::io::Result<Self> {
        let std_listener = match find_preallocated_fd(addr) {
            Some(listener) => listener,
            None => StdTcpListener::bind(addr)?,
        };

        std_listener.set_nonblocking(true)?;

        Ok(TcpListener::from_std(std_listener)?.into())
    }

    /// Sets the `TCP_NODELAY` option on the accepted connection.
    pub fn with_nodelay(self, nodelay: Option<bool>) -> Self {
        Self { nodelay, ..self }
    }

    /// Sets the `TCP_KEEPALIVE` option on the accepted connection.
    pub fn with_keepalive(self, keepalive_time: Option<Duration>) -> Self {
        Self {
            keepalive_time,
            keepalive: make_keepalive(
                keepalive_time,
                self.keepalive_interval,
                self.keepalive_retries,
            ),
            ..self
        }
    }

    /// Sets the `TCP_KEEPINTVL` option on the accepted connection.
    pub fn with_keepalive_interval(self, keepalive_interval: Option<Duration>) -> Self {
        Self {
            keepalive_interval,
            keepalive: make_keepalive(
                self.keepalive_time,
                keepalive_interval,
                self.keepalive_retries,
            ),
            ..self
        }
    }

    /// Sets the `TCP_KEEPCNT` option on the accepted connection.
    pub fn with_keepalive_retries(self, keepalive_retries: Option<u32>) -> Self {
        Self {
            keepalive_retries,
            keepalive: make_keepalive(
                self.keepalive_time,
                self.keepalive_interval,
                keepalive_retries,
            ),
            ..self
        }
    }

    /// Returns the local address that this tcp incoming is bound to.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.inner.as_ref().local_addr()
    }
}

impl From<TcpListener> for TcpIncoming {
    fn from(listener: TcpListener) -> Self {
        Self {
            inner: TcpListenerStream::new(listener),
            nodelay: None,
            keepalive: None,
            keepalive_time: None,
            keepalive_interval: None,
            keepalive_retries: None,
        }
    }
}

impl Stream for TcpIncoming {
    type Item = std::io::Result<TcpStream>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let polled = Pin::new(&mut self.inner).poll_next(cx);

        if let Poll::Ready(Some(Ok(stream))) = &polled {
            set_accepted_socket_options(stream, self.nodelay, &self.keepalive);
        }

        polled
    }
}

// Consistent with hyper-0.14, this function does not return an error.
fn set_accepted_socket_options(
    stream: &TcpStream,
    nodelay: Option<bool>,
    keepalive: &Option<TcpKeepalive>,
) {
    if let Some(nodelay) = nodelay
        && let Err(e) = stream.set_nodelay(nodelay)
    {
        warn!("error trying to set TCP_NODELAY: {e}");
    }

    if let Some(keepalive) = keepalive {
        let sock_ref = socket2::SockRef::from(&stream);
        if let Err(e) = sock_ref.set_tcp_keepalive(keepalive) {
            warn!("error trying to set TCP_KEEPALIVE: {e}");
        }
    }
}

fn make_keepalive(
    keepalive_time: Option<Duration>,
    keepalive_interval: Option<Duration>,
    keepalive_retries: Option<u32>,
) -> Option<TcpKeepalive> {
    let mut dirty = false;
    let mut keepalive = TcpKeepalive::new();
    if let Some(t) = keepalive_time {
        keepalive = keepalive.with_time(t);
        dirty = true;
    }

    #[cfg(
        // See https://docs.rs/socket2/0.5.8/src/socket2/lib.rs.html#511-525
        any(
            target_os = "android",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "fuchsia",
            target_os = "illumos",
            target_os = "ios",
            target_os = "visionos",
            target_os = "linux",
            target_os = "macos",
            target_os = "netbsd",
            target_os = "tvos",
            target_os = "watchos",
            target_os = "windows",
        )
    )]
    if let Some(t) = keepalive_interval {
        keepalive = keepalive.with_interval(t);
        dirty = true;
    }

    #[cfg(
        // See https://docs.rs/socket2/0.5.8/src/socket2/lib.rs.html#557-570
        any(
            target_os = "android",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "fuchsia",
            target_os = "illumos",
            target_os = "ios",
            target_os = "visionos",
            target_os = "linux",
            target_os = "macos",
            target_os = "netbsd",
            target_os = "tvos",
            target_os = "watchos",
        )
    )]
    if let Some(r) = keepalive_retries {
        keepalive = keepalive.with_retries(r);
        dirty = true;
    }

    // avoid clippy errors for targets that do not use these fields.
    let _ = keepalive_retries;
    let _ = keepalive_interval;

    dirty.then_some(keepalive)
}

// Adopts a socket-activation fd whose address matches `addr`, if one was passed in.
#[cfg(all(target_os = "linux", feature = "socket-activation"))]
fn find_preallocated_fd(addr: SocketAddr) -> Option<StdTcpListener> {
    use std::os::unix::io::FromRawFd;

    let fd = super::socket_activation::find_preallocated_fd(|fd| tcp_fd_matches(fd, addr))?;

    // SAFETY: `fd` is a validated, open activation descriptor. Ownership is taken
    // once here, the returned listener becomes its sole owner.
    Some(unsafe { StdTcpListener::from_raw_fd(fd) })
}

// Returns true if the listening socket at `fd` is bound to the requested address.
#[cfg(all(target_os = "linux", feature = "socket-activation"))]
fn tcp_fd_matches(fd: std::os::unix::io::RawFd, requested: SocketAddr) -> bool {
    use std::mem::ManuallyDrop;
    use std::os::unix::io::FromRawFd;

    // SAFETY: `fd` is a valid, open activation descriptor. `ManuallyDrop` keeps
    // ownership with the caller so it is not closed here, it is only borrowed to
    // read the bound address.
    let listener = ManuallyDrop::new(unsafe { StdTcpListener::from_raw_fd(fd) });
    matches!(listener.local_addr(), Ok(local) if socket_addr_matches(local, requested))
}

// Compares two socket addresses, treating IPv4-mapped and wildcard binds as equal.
#[cfg(all(target_os = "linux", feature = "socket-activation"))]
fn socket_addr_matches(inherited: SocketAddr, requested: SocketAddr) -> bool {
    use std::net::IpAddr;

    if inherited.port() != requested.port() {
        return false;
    }

    // Normalize IPv4-mapped IPv6 addresses to plain IPv4.
    fn normalize(ip: IpAddr) -> IpAddr {
        match ip {
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => IpAddr::V4(v4),
                None => IpAddr::V6(v6),
            },
            v4 => v4,
        }
    }

    let inherited_ip = normalize(inherited.ip());
    let requested_ip = normalize(requested.ip());

    if inherited_ip == requested_ip {
        return true;
    }

    requested_ip.is_unspecified() && inherited_ip.is_unspecified()
}

#[cfg(not(all(target_os = "linux", feature = "socket-activation")))]
fn find_preallocated_fd(_addr: SocketAddr) -> Option<StdTcpListener> {
    None
}

#[cfg(test)]
mod tests {
    use crate::transport::server::TcpIncoming;

    #[cfg(all(target_os = "linux", feature = "socket-activation"))]
    #[test]
    fn socket_addr_matches_cases() {
        use super::socket_addr_matches;

        let parse = |s: &str| -> std::net::SocketAddr { s.parse().unwrap() };

        assert!(socket_addr_matches(
            parse("127.0.0.1:50051"),
            parse("127.0.0.1:50051")
        ));

        assert!(!socket_addr_matches(
            parse("127.0.0.1:50051"),
            parse("127.0.0.1:1234")
        ));

        assert!(!socket_addr_matches(
            parse("127.0.0.1:50051"),
            parse("192.168.0.1:50051")
        ));

        assert!(socket_addr_matches(
            parse("[::]:50051"),
            parse("0.0.0.0:50051")
        ));
        assert!(socket_addr_matches(
            parse("0.0.0.0:50051"),
            parse("[::]:50051")
        ));

        assert!(socket_addr_matches(
            parse("[::ffff:127.0.0.1]:50051"),
            parse("127.0.0.1:50051")
        ));

        assert!(!socket_addr_matches(
            parse("127.0.0.1:50051"),
            parse("0.0.0.0:50051")
        ));
    }

    #[cfg(all(target_os = "linux", feature = "socket-activation"))]
    #[test]
    fn is_listening_stream_socket_cases() {
        use crate::transport::server::socket_activation::is_listening_stream_socket;
        use std::os::unix::io::AsRawFd;

        let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        assert!(is_listening_stream_socket(tcp.as_raw_fd()));

        let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        assert!(!is_listening_stream_socket(udp.as_raw_fd()));

        let raw = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::STREAM,
            Some(socket2::Protocol::TCP),
        )
        .unwrap();
        assert!(!is_listening_stream_socket(raw.as_raw_fd()));
    }

    #[tokio::test]
    async fn one_tcpincoming_at_a_time() {
        let addr = "127.0.0.1:1322".parse().unwrap();
        {
            let _t1 = TcpIncoming::bind(addr).unwrap();
            let _t2 = TcpIncoming::bind(addr).unwrap_err();
        }
        let _t3 = TcpIncoming::bind(addr).unwrap();
    }

    #[cfg(all(target_os = "linux", feature = "socket-activation"))]
    #[test]
    fn scan_adopts_matching_tcp_fd() {
        use super::tcp_fd_matches;
        use crate::transport::server::socket_activation::scan_preallocated_fds;
        use std::net::TcpListener as StdTcpListener;
        use std::os::unix::io::AsRawFd;

        let listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let fd = listener.as_raw_fd();

        let n_fds = fd - 2;
        let found = scan_preallocated_fds(std::process::id(), n_fds, |candidate| {
            tcp_fd_matches(candidate, addr)
        });

        assert_eq!(found, Some(fd));
    }
}
