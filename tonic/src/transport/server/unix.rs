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

use super::Connected;
use std::sync::Arc;
use std::{
    os::unix::net::UnixListener as StdUnixListener,
    path::Path,
    pin::Pin,
    task::{Context, Poll},
};

use tokio::net::{UnixListener, UnixStream};
use tokio_stream::{Stream, wrappers::UnixListenerStream};

/// Binds a Unix domain socket for a [Router](super::Router).
///
/// An incoming stream, usable with
/// [Router::serve_with_incoming](super::Router::serve_with_incoming), of
/// `AsyncRead + AsyncWrite` that communicate with clients that connect to a
/// Unix domain socket path.
#[derive(Debug)]
pub struct UnixIncoming {
    inner: UnixListenerStream,
}

impl UnixIncoming {
    /// Creates an instance by binding (opening) the specified socket path.
    ///
    /// Returns a `UnixIncoming` if the socket path was successfully bound.
    ///
    /// If the process was launched under a socket-activation manager
    /// that passed a listening Unix socket matching `path` via the
    /// `LISTEN_FDS` / `LISTEN_PID` environment variables, that inherited
    /// descriptor is adopted instead of opening a new socket. This behavior
    /// requires the `socket-activation` feature.
    ///
    /// # Examples
    /// ```no_run
    /// # use tower_service::Service;
    /// # use http::{request::Request, response::Response};
    /// # use tonic::{body::Body, server::NamedService, transport::{Server, server::UnixIncoming}};
    /// # use core::convert::Infallible;
    /// # use std::error::Error;
    /// # fn main() { }
    /// # fn run<S>(some_service: S) -> Result<(), Box<dyn Error + Send + Sync>>
    /// # where
    /// #   S: Service<Request<Body>, Response = Response<Body>, Error = Infallible> + NamedService + Clone + Send + Sync + 'static,
    /// #   S::Future: Send + 'static,
    /// # {
    /// let uinc = UnixIncoming::bind("/tmp/tonic/helloworld")?;
    /// Server::builder()
    ///    .add_service(some_service)
    ///    .serve_with_incoming(uinc);
    /// # Ok(())
    /// # }
    /// ```
    pub fn bind(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref();
        let std_listener = match find_preallocated_fd(path) {
            Some(listener) => listener,
            None => StdUnixListener::bind(path)?,
        };

        std_listener.set_nonblocking(true)?;

        Ok(UnixListener::from_std(std_listener)?.into())
    }

    /// Returns the local address that this Unix incoming is bound to.
    pub fn local_addr(&self) -> std::io::Result<tokio::net::unix::SocketAddr> {
        self.inner.as_ref().local_addr()
    }
}

impl From<UnixListener> for UnixIncoming {
    fn from(listener: UnixListener) -> Self {
        Self {
            inner: UnixListenerStream::new(listener),
        }
    }
}

impl Stream for UnixIncoming {
    type Item = std::io::Result<UnixStream>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

// Adopts a socket-activation fd bound to `path`, if one was passed in.
#[cfg(all(target_os = "linux", feature = "socket-activation"))]
fn find_preallocated_fd(path: &Path) -> Option<StdUnixListener> {
    use std::os::unix::io::FromRawFd;

    let fd = super::socket_activation::find_preallocated_fd(|fd| unix_fd_matches(fd, path))?;

    // SAFETY: `fd` is a validated, open activation descriptor. Ownership is taken
    // once here, the returned listener becomes its sole owner.
    Some(unsafe { StdUnixListener::from_raw_fd(fd) })
}

#[cfg(not(all(target_os = "linux", feature = "socket-activation")))]
fn find_preallocated_fd(_path: &Path) -> Option<StdUnixListener> {
    None
}

// Returns true if the listening socket at `fd` is bound to the requested path.
#[cfg(all(target_os = "linux", feature = "socket-activation"))]
fn unix_fd_matches(fd: std::os::unix::io::RawFd, requested: &Path) -> bool {
    use std::mem::ManuallyDrop;
    use std::os::unix::io::FromRawFd;

    // SAFETY: `fd` is a valid, open activation descriptor. `ManuallyDrop` keeps
    // ownership with the caller so it is not closed here; it is only borrowed to
    // read the bound address.
    let listener = ManuallyDrop::new(unsafe { StdUnixListener::from_raw_fd(fd) });
    matches!(listener.local_addr(), Ok(addr) if addr.as_pathname() == Some(requested))
}

/// Connection info for Unix domain socket streams.
///
/// This type will be accessible through [request extensions][ext] if you're using
/// a unix stream.
///
/// See [Connected] for more details.
///
/// [ext]: crate::Request::extensions
#[derive(Clone, Debug)]
pub struct UdsConnectInfo {
    /// Peer address. This will be "unnamed" for client unix sockets.
    pub peer_addr: Option<Arc<tokio::net::unix::SocketAddr>>,
    /// Process credentials for the unix socket.
    pub peer_cred: Option<tokio::net::unix::UCred>,
}

impl Connected for tokio::net::UnixStream {
    type ConnectInfo = UdsConnectInfo;

    fn connect_info(&self) -> Self::ConnectInfo {
        UdsConnectInfo {
            peer_addr: self.peer_addr().ok().map(Arc::new),
            peer_cred: self.peer_cred().ok(),
        }
    }
}

#[cfg(all(test, target_os = "linux", feature = "socket-activation"))]
mod tests {
    use std::os::unix::net::UnixListener as StdUnixListener;
    use std::path::PathBuf;

    fn temp_socket_path(tag: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "tonic-uds-test-{}-{}.sock",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn unix_fd_matches_cases() {
        use super::unix_fd_matches;
        use std::os::unix::io::AsRawFd;

        let path = temp_socket_path("matches");
        let other = temp_socket_path("matches-other");

        let listener = StdUnixListener::bind(&path).unwrap();
        let fd = listener.as_raw_fd();

        assert!(unix_fd_matches(fd, &path));
        assert!(!unix_fd_matches(fd, &other));

        drop(listener);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn is_listening_stream_socket_cases() {
        use crate::transport::server::socket_activation::is_listening_stream_socket;
        use std::os::unix::io::AsRawFd;

        let path = temp_socket_path("listening");

        let listener = StdUnixListener::bind(&path).unwrap();
        assert!(is_listening_stream_socket(listener.as_raw_fd()));

        let dgram = std::os::unix::net::UnixDatagram::unbound().unwrap();
        assert!(!is_listening_stream_socket(dgram.as_raw_fd()));

        let raw = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None).unwrap();
        assert!(!is_listening_stream_socket(raw.as_raw_fd()));

        drop(listener);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn scan_adopts_matching_unix_fd() {
        use super::unix_fd_matches;
        use crate::transport::server::socket_activation::scan_preallocated_fds;
        use std::os::unix::io::AsRawFd;

        let path = temp_socket_path("scan");
        let listener = StdUnixListener::bind(&path).unwrap();
        let fd = listener.as_raw_fd();

        let n_fds = fd - 2;
        let found = scan_preallocated_fds(std::process::id(), n_fds, |candidate| {
            unix_fd_matches(candidate, &path)
        });

        assert_eq!(found, Some(fd));

        drop(listener);
        let _ = std::fs::remove_file(&path);
    }
}
