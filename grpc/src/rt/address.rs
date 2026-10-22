/*
 *
 * Copyright 2026 gRPC authors.
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

use std::any::Any;
use std::fmt;

/// The address a [`Listener`](super::Listener) is bound to.
///
/// All implementations provide a human-readable string via [`Display`].
/// Callers needing structured data (e.g. `TcpAddress` or `std::net::SocketAddr`) can
/// downcast via stable trait upcasting coercion (`let any: &dyn Any = addr;`)
/// and [`downcast_ref`](Any::downcast_ref).
///
/// This is analogous to Go's `net.Addr` interface.
pub trait ListenerAddress: fmt::Debug + Any + Send + Sync + 'static {
    /// Returns the network type (e.g. `"tcp"`, `"unix"`, `"inmemory"`).
    fn network(&self) -> &str;
}

// ---------------------------------------------------------------------------
// TcpAddress (Scheme-Prefixed TCP Address Wrapper)
// ---------------------------------------------------------------------------

/// A wrapper around [`std::net::SocketAddr`] used as a listener address.
/// `Debug`-formats as `TcpAddress(<ip>:<port>)` for logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TcpAddress(pub std::net::SocketAddr);

impl ListenerAddress for TcpAddress {
    fn network(&self) -> &str {
        "tcp"
    }
}

// ---------------------------------------------------------------------------
// UnixListenerAddress
// ---------------------------------------------------------------------------

/// Address for a Unix socket listener.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct UnixListenerAddress {
    path: String,
}

impl UnixListenerAddress {
    /// Creates a new Unix listener address.
    pub(crate) fn new(path: String) -> Self {
        Self { path }
    }

    /// Returns the socket path.
    pub(crate) fn path(&self) -> &str {
        &self.path
    }
}

impl ListenerAddress for UnixListenerAddress {
    fn network(&self) -> &str {
        "unix"
    }
}

/// Address representing a listener whose local address could not be determined.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FailingSocketAddress {
    network: &'static str,
    error: String,
}

impl FailingSocketAddress {
    /// Creates a new `FailingSocketAddress` with the specified network and error description.
    pub fn new(network: &'static str, error: String) -> Self {
        Self { network, error }
    }

    /// Returns the underlying error message describing why obtaining the address failed.
    pub fn error(&self) -> &str {
        &self.error
    }
}

impl ListenerAddress for FailingSocketAddress {
    fn network(&self) -> &str {
        self.network
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use super::*;

    // -- TcpAddress as ListenerAddress --

    #[test]
    fn tcp_address_network_is_tcp() {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let tcp = TcpAddress(addr);
        assert_eq!(tcp.network(), "tcp");
    }

    #[test]
    fn tcp_address_debug_contains_socket_addr() {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let tcp = TcpAddress(addr);
        assert!(format!("{tcp:?}").contains("127.0.0.1:8080"));
    }

    #[test]
    fn tcp_address_downcast_via_any_supertrait() {
        let addr: SocketAddr = "[::1]:443".parse().unwrap();
        let tcp = TcpAddress(addr);
        let trait_obj: &dyn ListenerAddress = &tcp;
        let any_ref: &dyn Any = trait_obj;
        let downcasted = any_ref
            .downcast_ref::<TcpAddress>()
            .expect("downcast failed");
        assert_eq!(*downcasted, tcp);
    }

    // -- UnixListenerAddress --

    #[test]
    fn unix_address_path() {
        let addr = UnixListenerAddress::new("/tmp/grpc.sock".to_string());
        assert_eq!(addr.path(), "/tmp/grpc.sock");
    }

    #[test]
    fn unix_address_network_is_unix() {
        let addr = UnixListenerAddress::new("/tmp/grpc.sock".to_string());
        assert_eq!(addr.network(), "unix");
    }

    #[test]
    fn unix_address_debug_contains_path() {
        let addr = UnixListenerAddress::new("/var/run/server.sock".to_string());
        assert!(format!("{addr:?}").contains("/var/run/server.sock"));
    }

    #[test]
    fn unix_address_downcast_via_any_supertrait() {
        let addr = UnixListenerAddress::new("/tmp/test.sock".to_string());
        let trait_obj: &dyn ListenerAddress = &addr;
        let any_ref: &dyn Any = trait_obj;
        let downcasted = any_ref
            .downcast_ref::<UnixListenerAddress>()
            .expect("downcast failed");
        assert_eq!(downcasted.path(), "/tmp/test.sock");
    }

    #[test]
    fn failing_address_network() {
        let addr = FailingSocketAddress::new("tcp", "permission denied".to_string());
        assert_eq!(addr.network(), "tcp");
        assert_eq!(addr.error(), "permission denied");
    }

    #[test]
    fn failing_address_debug_contains_network_and_error() {
        let addr = FailingSocketAddress::new("unix", "socket unbound".to_string());
        let debug_str = format!("{addr:?}");
        assert!(debug_str.contains("unix"));
        assert!(debug_str.contains("socket unbound"));
    }

    #[test]
    fn failing_address_downcast_via_any_supertrait() {
        let addr = FailingSocketAddress::new("tcp", "bad address".to_string());
        let trait_obj: &dyn ListenerAddress = &addr;
        let any_ref: &dyn Any = trait_obj;
        let downcasted = any_ref
            .downcast_ref::<FailingSocketAddress>()
            .expect("downcast failed");
        assert_eq!(downcasted.error(), "bad address");
        assert!(any_ref.downcast_ref::<TcpAddress>().is_none());
    }
}
