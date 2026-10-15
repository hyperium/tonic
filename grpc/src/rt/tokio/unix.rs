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

//! Unix domain socket support for the tokio runtime.

use std::path::PathBuf;

use tokio::net::UnixListener;
use tokio::net::UnixStream;
use tokio::net::unix::SocketAddr;

use crate::client::name_resolution::UNIX_NETWORK_TYPE;
use crate::rt::EndpointListener;
use crate::rt::GrpcEndpoint;
use crate::rt::StreamEndpoint;
use crate::rt::address::FailingSocketAddress;
use crate::rt::address::ListenerAddress;
use crate::rt::address::UnixListenerAddress;
use crate::rt::tokio::TOKIO_ACCEPT_RETRY_POLICY;
use crate::rt::tokio::accept_with_retry;

/// Connects to the Unix socket at `path`.
pub async fn connect(path: PathBuf) -> Result<Box<dyn GrpcEndpoint>, String> {
    let stream = UnixStream::connect(&path)
        .await
        .map_err(|err| err.to_string())?;
    endpoint(stream)
}

/// Binds a listener to the Unix socket at `path`.
pub fn bind(path: PathBuf) -> Result<Box<dyn EndpointListener>, String> {
    let listener = UnixListener::bind(&path).map_err(|e| e.to_string())?;
    Ok(Box::new(TokioUnixListener { listener }))
}

fn endpoint(stream: UnixStream) -> Result<Box<dyn GrpcEndpoint>, String> {
    let peer_addr = stream.peer_addr().map_err(|e| e.to_string())?;
    let local_addr = stream.local_addr().map_err(|e| e.to_string())?;
    Ok(Box::new(StreamEndpoint {
        peer_addr: address_string(&peer_addr).into_boxed_str(),
        local_addr: address_string(&local_addr).into_boxed_str(),
        network_type: UNIX_NETWORK_TYPE,
        inner: stream,
    }))
}

/// Creates an endpoint for a stream returned by `accept`, which also returned
/// `peer_addr`. If the local address can't be read, it is left empty.
fn accepted_endpoint(stream: UnixStream, peer_addr: SocketAddr) -> Box<dyn GrpcEndpoint> {
    let local_addr = stream
        .local_addr()
        .map(|addr| address_string(&addr))
        .unwrap_or_default();
    Box::new(StreamEndpoint {
        peer_addr: address_string(&peer_addr).into_boxed_str(),
        local_addr: local_addr.into_boxed_str(),
        network_type: UNIX_NETWORK_TYPE,
        inner: stream,
    })
}

/// Formats a Unix socket address like gRPC C-core and name resolution do: the
/// path for pathname sockets, `\0` followed by the name for abstract sockets,
/// and an empty string for unnamed sockets.
///
/// Non-UTF-8 paths and names are converted lossily.
fn address_string(addr: &SocketAddr) -> String {
    if let Some(path) = addr.as_pathname() {
        return path.to_string_lossy().into_owned();
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    if let Some(name) = addr.as_abstract_name() {
        return format!("\0{}", String::from_utf8_lossy(name));
    }
    String::new()
}

/// Wraps `tokio::net::UnixListener` as an [`EndpointListener`].
struct TokioUnixListener {
    listener: UnixListener,
}

#[crate::async_trait]
impl EndpointListener for TokioUnixListener {
    async fn accept(&self) -> Result<Box<dyn GrpcEndpoint>, String> {
        let (stream, peer_addr) =
            accept_with_retry(&TOKIO_ACCEPT_RETRY_POLICY, || self.listener.accept()).await;
        Ok(accepted_endpoint(stream, peer_addr))
    }

    fn local_addr(&self) -> Box<dyn ListenerAddress> {
        // TODO: Should the API return result or a FailingAddress type?
        match self.listener.local_addr().map_err(|e| e.to_string()) {
            Ok(addr) => Box::new(UnixListenerAddress::new(address_string(&addr))),
            Err(err) => Box::new(FailingSocketAddress::new("unix", err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::address_string;

    #[test]
    fn address_string_formats_pathname_as_path() {
        let addr = std::os::unix::net::SocketAddr::from_pathname("/tmp/grpc.sock").unwrap();
        assert_eq!(address_string(&addr.into()), "/tmp/grpc.sock");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn address_string_formats_abstract_name_with_leading_nul() {
        use std::os::linux::net::SocketAddrExt;

        let addr = std::os::unix::net::SocketAddr::from_abstract_name(b"grpc-test").unwrap();
        assert_eq!(address_string(&addr.into()), "\0grpc-test");
    }

    #[tokio::test]
    async fn address_string_formats_unnamed_as_empty() {
        let (stream, _peer) = tokio::net::UnixStream::pair().unwrap();
        let addr = stream.local_addr().unwrap();
        assert_eq!(address_string(&addr), "");
    }
}
