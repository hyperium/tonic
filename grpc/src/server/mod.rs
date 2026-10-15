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

//! Server-side gRPC implementation and utilities.
//!
//! This module provides the core types and traits for building gRPC servers.
//! While most applications will use generated code (e.g. using
//! [`protoc-gen-rust-grpc`](https://crates.io/protoc-gen-rust-grpc)) to
//! interact with gRPC services, this module provides the underlying primitives.
//!
//! # Key Concepts
//!
//! - **[`Server`]:** The main entry point for server-side gRPC operations.
//! - **[`Listener`]:** A trait for accepting incoming RPCs.
//! - **[`Handle`]:** A trait implemented by services to handle incoming RPCs.
//!
//! # Additional Types
//!
//! - **[`SendStream`] / [`RecvStream`]:** Represent the sending and receiving
//!   sides of a server-side RPC.
//! - **[`RequestHeaders`]:** Represents gRPC headers sent by the client to
//!   initiate a request.
//! - **[`ResponseHeaders`] / [`Trailers`]:** Represent gRPC headers and
//!   trailers sent to the client during the server's response.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::watch;

use crate::async_trait;
use crate::core::ConnectionInfo;
use crate::core::RecvMessage;
use crate::core::SendMessage;
use crate::metadata::AsciiMetadataValue;
use crate::metadata::MetadataMap;
use crate::private::Internal;
use crate::rt::GrpcRuntime;
use crate::rt::address::ListenerAddress;
use crate::send_future::SendFuture;

pub mod builder;
pub mod descriptor;
pub(crate) mod http2_config;
pub mod interceptor;
pub(crate) mod router;
pub mod service;
pub(crate) mod transport;

use builder::ServerBuilder;
use interceptor::Identity;
pub use http2_config::Http2Config;

/// Settings to configure RPCs sent using the [`Handle`] trait.
///
/// Most applications will not need this type, and will set options via the
/// generated (e.g. protobuf) APIs instead.
#[derive(Default, Clone)]
#[non_exhaustive]
pub struct CallOptions {}

impl CallOptions {
    /// Constructs a new [`CallOptions`] with the default settings.
    pub fn new() -> Self {
        Self::default()
    }
}

/// A serving connection that supports graceful shutdown.
///
/// Implements `Future` to drive the connection, and exposes
/// `graceful_shutdown()` to initiate a graceful drain.
///
/// # Contract
/// Implementors MUST drive all of the connection's work within this future
/// (or own the handles to any sub-tasks they spawn). Dropping the connection
/// future MUST cancel all associated work and release the socket. Detaching
/// work via a fire-and-forget spawn breaks force-shutdown and leaks tasks.
///
/// Furthermore, `graceful_shutdown()` MUST be idempotent: calling it multiple
/// times on the same connection MUST be safe and act as a no-op after the
/// initial shutdown signal is sent.
#[doc(hidden)]
pub trait GracefulConnection: Future<Output = Result<(), String>> + Send + 'static {
    /// Initiate graceful shutdown.
    fn graceful_shutdown(self: Pin<&mut Self>, token: crate::private::Internal);
}

#[derive(Default)]
pub struct Server {
    handler: Option<Arc<dyn DynHandle>>,
    runtime: GrpcRuntime,
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for crate::inmemory::InMemoryListener {}
    impl Sealed for crate::server::transport::hyper::HyperListener {}
}

/// A bound listening socket that yields incoming connections.
///
/// # Shutdown and drop semantics
///
/// When a `Listener` is dropped (for example, immediately when a shutdown signal
/// fires or when the server stops accepting connections), dropping the listener
/// closes the underlying socket/channel and rejects new incoming connections
/// immediately, while existing accepted connections continue to run and drain
/// independently.
///
/// **Sealed** — external crates cannot implement this trait.
#[trait_variant::make(Send)]
pub trait Listener: sealed::Sealed {
    /// The concrete transport type yielded by this listener.
    type Transport: Transport + 'static;

    /// Accepts the next incoming connection.
    ///
    /// Returns an error once the listener can't produce any more connections,
    /// because it was closed or because accepting failed. The server then stops
    /// accepting and returns that error.
    #[doc(hidden)]
    async fn accept(&self, token: Internal) -> Result<Self::Transport, String>;

    /// Returns the address this listener is bound to.
    fn local_addr(&self) -> Box<dyn ListenerAddress>;
}

/// A connection accepted by a [`Listener`] that can serve RPCs.
#[doc(hidden)]
pub trait Transport: Send + 'static {
    /// The serving connection type.
    type Connection: GracefulConnection;

    /// Bind the handler and start serving.
    /// Returns a [`GracefulConnection`] that drives the connection when polled.
    fn serve(
        self,
        handler: Arc<dyn DynHandle>,
        runtime: GrpcRuntime,
        token: crate::private::Internal,
    ) -> Self::Connection;
}

/// Coordinates graceful shutdown across all spawned connections.
///
/// Each connection is watched via `watch()`. When `shutdown()` is called,
/// all watched connections receive a `graceful_shutdown()` signal.
struct GracefulCoordinator {
    tx: watch::Sender<()>,
}

impl GracefulCoordinator {
    fn new() -> Self {
        let (tx, _) = watch::channel(());
        Self { tx }
    }

    fn watch<C: GracefulConnection>(
        &self,
        conn: C,
    ) -> impl Future<Output = Result<(), String>> + Send + 'static {
        let mut rx = self.tx.subscribe();
        async move {
            let mut conn = std::pin::pin!(conn);
            loop {
                tokio::select! {
                    res = &mut conn => { return res; }
                    res = rx.changed() => {
                        match res {
                            // Operator graceful signal: begin GOAWAY drain,
                            // keep polling the connection to completion.
                            Ok(()) => conn.as_mut().graceful_shutdown(crate::private::Internal),
                            // Sender dropped == serve future dropped (force
                            // shutdown): stop now. Dropping `conn` closes the
                            // socket and cancels in-flight work.
                            Err(_) => return Ok(()),
                        }
                    }
                }
            }
        }
    }

    async fn shutdown(self) {
        let _ = self.tx.send(());
        self.tx.closed().await;
    }
}

impl Server {
    /// Creates a new [`ServerBuilder`] with a no-op interceptor.
    pub fn builder() -> ServerBuilder<Identity> {
        ServerBuilder::new()
    }

    /// Binds a TCP listener to `addr` with the given server credentials and HTTP/2 configuration.
    // TODO: Consider making this a builder to allow adding additional listener
    // configuration without breaking changes.
    #[cfg(feature = "_runtime-tokio")]
    pub async fn tcp_listener(
        addr: std::net::SocketAddr,
        creds: Arc<dyn crate::credentials::ServerCredentials>,
        config: Http2Config,
    ) -> Result<impl Listener, String> {
        let listener = transport::hyper::HyperListener::new_tcp_listener(
            addr,
            creds,
            &crate::rt::default_runtime(),
        )
        .await?;
        Ok(listener.with_config(config))
    }

    /// Creates a new server with the given handler and runtime.
    pub(crate) fn new(handler: impl Handle + 'static, runtime: GrpcRuntime) -> Self {
        Self {
            handler: Some(Arc::new(handler)),
            runtime,
        }
    }

    /// Returns the runtime used by this server.
    pub fn runtime(&self) -> &GrpcRuntime {
        &self.runtime
    }

    /// Serves on the given listener until accepting fails.
    ///
    /// After the accept loop ends, the listener is dropped, all open HTTP/2
    /// connections receive a GOAWAY frame, and the server waits for in-flight
    /// RPCs to finish before returning.
    ///
    /// # Errors
    ///
    /// This only returns once the listener fails to accept: the server drains as
    /// above and then returns the listener's error. Draining has no time limit.
    // TODO: Revisit the serve and accept error API, especially the error type, before GA.
    pub async fn serve(&self, listener: impl Listener) -> Result<(), String> {
        self.serve_with_shutdown(listener, std::future::pending())
            .await
    }

    /// Serves on the given listener until `signal` resolves or the listener
    /// fails, then drains all in-flight connections before returning.
    ///
    /// When `signal` completes:
    /// 1. The server stops accepting new connections and drops the listener,
    ///    immediately closing the listening socket.
    /// 2. All open HTTP/2 connections receive a GOAWAY frame.
    /// 3. In-flight RPCs continue to completion.
    /// 4. This method returns once all connections are closed.
    ///
    /// If the listener fails to accept, the server drains the same way and
    /// then returns the listener's error.
    ///
    /// Draining has no time limit. To bound it, drop the returned future, for
    /// example by racing it against a timer; dropping it closes all remaining
    /// connections.
    pub(crate) async fn serve_with_shutdown(
        &self,
        listener: impl Listener,
        signal: impl Future<Output = ()>,
    ) -> Result<(), String> {
        let graceful = GracefulCoordinator::new();
        let mut exit_err: Option<String> = None;

        tokio::pin!(signal);

        loop {
            tokio::select! {
                conn = listener.accept(crate::private::Internal) => {
                    let connection = match conn {
                        Ok(c) => c,
                        Err(err) => {
                            exit_err = Some(err);
                            break;
                        }
                    };
                    let handler = match self.handler.as_ref() {
                        Some(h) => h.clone(),
                        None => continue,
                    };
                    let serving = connection.serve(
                        handler,
                        self.runtime.clone(),
                        crate::private::Internal,
                    );
                    let fut = graceful.watch(serving);
                    self.runtime.spawn(Box::pin(async move {
                        if let Err(_err) = fut.await {
                            // TODO: Perform logging or some sort of connection error signal for connection errors.
                        }
                    }));
                }
                _ = &mut signal => {
                    break;
                }
            }
        }

        drop(listener);

        graceful.shutdown().await;

        match exit_err {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }
}

/// A trait which may be implemented by types to handle server-side logic of
/// RPCs (Remote Procedure Calls, often shortened to "call").
#[trait_variant::make(Send)]
pub trait Handle: Send + Sync {
    /// Handles an RPC, accepting the send and receive streams that are used to
    /// interact with the call.  Note that `tx` is not static, so it cannot be
    /// sent to another task, meaning the RPC must end before handle returns.
    async fn handle(
        &self,
        headers: RequestHeaders,
        options: CallOptions,
        tx: &mut impl SendStream,
        rx: impl RecvStream + 'static,
    ) -> Trailers;
}

#[doc(hidden)]
#[async_trait]
pub trait DynHandle: Send + Sync {
    async fn dyn_handle(
        &self,
        headers: RequestHeaders,
        options: CallOptions,
        tx: &mut dyn DynSendStream,
        rx: BoxedRecvStream,
    ) -> Trailers;
}

#[async_trait]
impl<T: Handle> DynHandle for T {
    async fn dyn_handle(
        &self,
        headers: RequestHeaders,
        options: CallOptions,
        mut tx: &mut dyn DynSendStream,
        rx: BoxedRecvStream,
    ) -> Trailers {
        self.handle(headers, options, &mut tx, rx).make_send().await
    }
}

pub(crate) type BoxedRecvStream = Box<dyn DynRecvStream>;

/// An item in a response stream from the server's view.
///
/// These items are sent to the client via a [`SendStream`], using references to
/// avoid allocations.
pub enum ResponseStreamItem<'a> {
    /// Indicates the headers for the stream.
    Headers(ResponseHeaders),
    /// Indicates a message on the stream.
    Message(&'a dyn SendMessage),
}

/// Bridges [`DynHandle`] (object-safe) back to [`Handle`] (generic),
/// enabling interceptor composition on top of a dynamic handler.
pub(crate) struct DynHandleWrapper(pub Arc<dyn DynHandle>);

impl Handle for DynHandleWrapper {
    async fn handle(
        &self,
        headers: RequestHeaders,
        options: CallOptions,
        tx: &mut impl SendStream,
        rx: impl RecvStream + 'static,
    ) -> Trailers {
        self.0.dyn_handle(headers, options, tx, Box::new(rx)).await
    }
}
/// Represents the sending side of a server stream.  See `ResponseStream`
/// documentation for information about the different types of items and the
/// order in which they must be sent.
#[trait_variant::make(Send)]
pub trait SendStream {
    /// Sends the next item on the stream. Returns `Ok(())` on success, or
    /// `Err(())` on failure. `Err(())` is a terminal state.
    /// Calling this method after an error should be avoided and is unspecified.
    ///
    /// # Cancel safety
    ///
    /// This method is not intended to be cancellation safe.  If the returned
    /// future is not polled to completion, the behavior of any subsequent calls
    /// to the SendStream are undefined and data may be lost.
    async fn send(&mut self, item: ResponseStreamItem<'_>, options: SendOptions) -> Result<(), ()>;
}

#[doc(hidden)]
#[async_trait]
pub trait DynSendStream: Send {
    async fn dyn_send<'a>(
        &mut self,
        item: ResponseStreamItem<'a>,
        options: SendOptions,
    ) -> Result<(), ()>;
}

#[async_trait]
impl<T: SendStream> DynSendStream for T {
    async fn dyn_send<'a>(
        &mut self,
        item: ResponseStreamItem<'a>,
        options: SendOptions,
    ) -> Result<(), ()> {
        self.send(item, options).await
    }
}

impl SendStream for &mut (dyn DynSendStream + '_) {
    async fn send(&mut self, item: ResponseStreamItem<'_>, options: SendOptions) -> Result<(), ()> {
        (**self).dyn_send(item, options).await
    }
}

impl SendStream for Box<dyn DynSendStream + '_> {
    async fn send(&mut self, item: ResponseStreamItem<'_>, options: SendOptions) -> Result<(), ()> {
        (**self).dyn_send(item, options).await
    }
}

/// Contains settings to configure a send operation on a SendStream.
#[derive(Default)]
#[non_exhaustive]
pub struct SendOptions {
    /// Delays sending the message until the trailers are provided on the stream
    /// and batches the two items together if possible.
    pub final_msg: bool,
    /// If set, compression will be disabled for this message.
    pub disable_compression: bool,
}

/// Represents the receiving side of a server stream.
#[trait_variant::make(Send)]
pub trait RecvStream {
    /// Returns the next message on the stream. Returns `Some(Ok(()))` on
    /// success, `None` on normal stream end, or `Some(Err(()))` if the stream
    /// encountered an error before the client's final request message. Both
    /// `None` and `Some(Err(()))` are terminal states.
    /// Calling this method again after reaching a terminal state is unspecified
    /// and should be avoided.
    ///
    /// The provided `msg` must not be modified if the stream has ended, i.e. `None`
    /// is returned.
    ///
    /// # Cancel safety
    ///
    /// This method is not intended to be cancellation safe.  If the returned
    /// future is not polled to completion, the behavior of any subsequent calls
    /// to the RecvStream are undefined and data may be lost.
    async fn next(&mut self, msg: &mut dyn RecvMessage) -> Option<Result<(), ()>>;
}

#[doc(hidden)]
#[async_trait]
pub trait DynRecvStream: Send {
    async fn dyn_next(&mut self, msg: &mut dyn RecvMessage) -> Option<Result<(), ()>>;
}

#[async_trait]
impl<T: RecvStream> DynRecvStream for T {
    async fn dyn_next(&mut self, msg: &mut dyn RecvMessage) -> Option<Result<(), ()>> {
        self.next(msg).await
    }
}

impl RecvStream for Box<dyn DynRecvStream + '_> {
    async fn next(&mut self, msg: &mut dyn RecvMessage) -> Option<Result<(), ()>> {
        (**self).dyn_next(msg).await
    }
}

/// Contains all information transmitted in the response headers of an RPC.
#[derive(Debug, Clone, Default)]
pub struct ResponseHeaders {
    metadata: MetadataMap,
}

impl ResponseHeaders {
    /// Returns a default ResponseHeaders instance.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the metadata of self with `metadata`.
    pub fn with_metadata(mut self, metadata: MetadataMap) -> Self {
        self.metadata = metadata;
        self
    }

    /// Returns a reference to the metadata in these headers.
    pub fn metadata(&self) -> &MetadataMap {
        &self.metadata
    }

    /// Returns a mutable reference to the metadata in these headers.
    pub fn metadata_mut(&mut self) -> &mut MetadataMap {
        &mut self.metadata
    }

    pub(crate) fn into_metadata(self) -> MetadataMap {
        self.metadata
    }
}

/// Contains all information transmitted in the request headers of an RPC.
// TODO: Revisit symmetry with `client::CallOptions` (whether timeout/deadline
// and compression encodings should live on `server::RequestHeaders` or `server::CallOptions`).
#[derive(Debug, Clone)]
pub struct RequestHeaders {
    /// The full (e.g. "/Service/Method") method name specified for the call.
    method_name: String,
    /// The application-specified metadata for the call.
    metadata: MetadataMap,
    /// Information about the client.
    connection_info: ConnectionInfo,
    /// The deadline for the call, derived from the client's `grpc-timeout`
    /// when the call started.
    deadline: Option<Instant>,
    /// The message compression encoding (`grpc-encoding`) specified by the client.
    encoding: Option<AsciiMetadataValue>,
    /// The accepted response compression encodings (`grpc-accept-encoding`) specified by the client.
    accept_encoding: Option<AsciiMetadataValue>,
}

impl RequestHeaders {
    /// Returns a default RequestHeaders instance.
    pub fn new(method_name: impl Into<String>, connection_info: ConnectionInfo) -> Self {
        Self {
            method_name: method_name.into(),
            connection_info,
            metadata: MetadataMap::default(),
            deadline: None,
            encoding: None,
            accept_encoding: None,
        }
    }

    /// Replaces the method name of self with `method_name`.
    pub fn with_method_name(mut self, method_name: impl Into<String>) -> Self {
        self.method_name = method_name.into();
        self
    }

    /// Replaces the metadata of self with `metadata`.
    pub fn with_metadata(mut self, metadata: MetadataMap) -> Self {
        self.metadata = metadata;
        self
    }

    /// Returns the full (e.g. "/Service/Method") method name for these headers.
    pub fn method_name(&self) -> &str {
        &self.method_name
    }

    /// Returns a reference to the metadata in these headers.
    pub fn metadata(&self) -> &MetadataMap {
        &self.metadata
    }

    /// Returns a mutable reference to the metadata in these headers.
    pub fn metadata_mut(&mut self) -> &mut MetadataMap {
        &mut self.metadata
    }

    /// Replaces the connection_info of self with `connection_info`.
    pub fn with_connection_info(mut self, connection_info: ConnectionInfo) -> Self {
        self.connection_info = connection_info;
        self
    }

    /// Returns a reference to the connection_info in these headers.
    pub fn connection_info(&self) -> &ConnectionInfo {
        &self.connection_info
    }

    /// Sets the deadline for the call on these headers.
    pub fn with_deadline(mut self, deadline: Instant) -> Self {
        self.deadline = Some(deadline);
        self
    }

    /// Returns the deadline for the call, if the client sent a `grpc-timeout`.
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Sets the `grpc-encoding` value on these headers.
    pub fn with_encoding(mut self, encoding: AsciiMetadataValue) -> Self {
        self.encoding = Some(encoding);
        self
    }

    /// Returns the `grpc-encoding` header value sent by the client, if present.
    pub fn encoding(&self) -> Option<&str> {
        self.encoding.as_ref().map(|v| v.to_str())
    }

    /// Sets the `grpc-accept-encoding` value on these headers.
    pub fn with_accept_encoding(mut self, accept_encoding: AsciiMetadataValue) -> Self {
        self.accept_encoding = Some(accept_encoding);
        self
    }

    /// Returns the `grpc-accept-encoding` header value sent by the client, if present.
    pub fn accept_encoding(&self) -> Option<&str> {
        self.accept_encoding.as_ref().map(|v| v.to_str())
    }
}

/// Contains all information transmitted in the response trailers of an RPC.
/// gRPC does not support request trailers.
#[derive(Debug, Clone)]
pub struct Trailers {
    status: crate::Result<()>,
    metadata: MetadataMap,
}

impl Trailers {
    /// Returns a default [`Trailers`] instance.
    pub fn new(status: crate::Result<()>) -> Self {
        Self {
            status,
            metadata: MetadataMap::default(),
        }
    }

    /// Replaces the status of self with `status`.
    pub fn with_status(mut self, status: crate::Result<()>) -> Self {
        self.status = status;
        self
    }

    /// Returns a reference to the status contained in these trailers.
    pub fn status(&self) -> &crate::Result<()> {
        &self.status
    }

    /// Replaces the metadata of self with `metadata`.
    pub fn with_metadata(mut self, metadata: MetadataMap) -> Self {
        self.metadata = metadata;
        self
    }

    /// Returns a mutable reference to the metadata in these trailers.
    pub fn metadata_mut(&mut self) -> &mut MetadataMap {
        &mut self.metadata
    }

    /// Returns a reference to the metadata in these trailers.
    pub fn metadata(&self) -> &MetadataMap {
        &self.metadata
    }

    /// Returns the status in the [`Trailers`], consuming the entire status.
    pub fn into_status(self) -> crate::Result<()> {
        self.status
    }

    pub(crate) fn into_parts(self) -> (crate::Result<()>, MetadataMap) {
        (self.status, self.metadata)
    }
}

#[cfg(test)]
mod tests {
    use std::any::Any;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;
    use std::task::Context;
    use std::task::Poll;
    use std::time::Duration;

    use tokio::sync::oneshot;
    use tokio::time::timeout;

    use super::*;
    use crate::core::test_connection_info;
    use crate::credentials::LocalServerCredentials;
    use crate::credentials::ServerCredentials;
    use crate::rt::address::TcpAddress;
    /// A mock connection with one RPC in flight. It finishes only when the test
    /// sends on its finish sender, even after [`graceful_shutdown`], which it
    /// reports on its shutdown receiver.
    struct MockConnection {
        shutdown_called: Option<oneshot::Sender<()>>,
        finish: oneshot::Receiver<()>,
    }

    impl MockConnection {
        /// Returns the connection, a receiver that resolves when
        /// `graceful_shutdown` is called, and a sender that finishes the
        /// connection.
        fn new() -> (Self, oneshot::Receiver<()>, oneshot::Sender<()>) {
            let (shutdown_tx, shutdown_rx) = oneshot::channel();
            let (finish_tx, finish_rx) = oneshot::channel();
            (
                Self {
                    shutdown_called: Some(shutdown_tx),
                    finish: finish_rx,
                },
                shutdown_rx,
                finish_tx,
            )
        }
    }

    impl Future for MockConnection {
        type Output = Result<(), String>;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), String>> {
            Pin::new(&mut self.finish).poll(cx).map(|_| Ok(()))
        }
    }

    impl GracefulConnection for MockConnection {
        fn graceful_shutdown(mut self: Pin<&mut Self>, _token: crate::private::Internal) {
            if let Some(shutdown_called) = self.shutdown_called.take() {
                let _ = shutdown_called.send(());
            }
        }
    }

    #[tokio::test]
    async fn shutdown_signals_watched_connection() {
        let coordinator = GracefulCoordinator::new();
        let (conn, shutdown_called, finish) = MockConnection::new();

        let watched = coordinator.watch(conn);
        let handle = tokio::spawn(watched);

        tokio::task::yield_now().await;

        let shutdown = tokio::spawn(coordinator.shutdown());
        shutdown_called.await.unwrap();
        assert!(!shutdown.is_finished());
        finish.send(()).unwrap();

        handle.await.unwrap().unwrap();
        shutdown.await.unwrap();
    }

    #[tokio::test]
    async fn connection_completes_before_shutdown() {
        let coordinator = GracefulCoordinator::new();
        let (conn, shutdown_called, finish) = MockConnection::new();

        let watched = coordinator.watch(conn);
        let handle = tokio::spawn(watched);
        tokio::task::yield_now().await;

        // Connection finishes on its own (e.g. client disconnected).
        finish.send(()).unwrap();
        handle.await.unwrap().unwrap();

        // graceful_shutdown was never called.
        assert!(shutdown_called.await.is_err());

        // Shutdown still returns promptly (no connections to drain).
        coordinator.shutdown().await;
    }

    #[tokio::test]
    async fn two_connections_both_drained() {
        let coordinator = GracefulCoordinator::new();

        let (conn_a, shutdown_a, finish_a) = MockConnection::new();
        let (conn_b, shutdown_b, finish_b) = MockConnection::new();

        let handle_a = tokio::spawn(coordinator.watch(conn_a));
        let handle_b = tokio::spawn(coordinator.watch(conn_b));

        tokio::task::yield_now().await;
        let shutdown = tokio::spawn(coordinator.shutdown());
        shutdown_a.await.unwrap();
        shutdown_b.await.unwrap();
        finish_a.send(()).unwrap();
        finish_b.send(()).unwrap();

        handle_a.await.unwrap().unwrap();
        handle_b.await.unwrap().unwrap();
        shutdown.await.unwrap();
    }

    // -- Server + InMemoryListener integration tests --

    #[tokio::test]
    async fn server_stops_on_shutdown_signal() {
        let listener = crate::inmemory::InMemoryListener::new();
        let server = Server::builder().build();

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let listener_clone = listener.clone();
        let server_handle = tokio::spawn(async move {
            server
                .serve_with_shutdown(listener_clone, async {
                    let _ = shutdown_rx.await;
                })
                .await
                .unwrap();
        });

        // Fire shutdown immediately.
        let _ = shutdown_tx.send(());

        // Server should exit promptly.
        tokio::time::timeout(std::time::Duration::from_secs(2), server_handle)
            .await
            .expect("server did not shut down within 2s")
            .unwrap();
    }

    #[tokio::test]
    async fn dropped_sender_breaks_watch_loop() {
        let coordinator = GracefulCoordinator::new();
        let (conn, shutdown_called, _finish) = MockConnection::new();

        let watched = coordinator.watch(conn);
        let handle = tokio::spawn(watched);

        tokio::task::yield_now().await;

        // Drop the coordinator/Sender explicitly (simulating serve future being dropped).
        drop(coordinator);

        // The watched task should exit promptly because `rx.changed()` returns `Err`, so the watcher returns `Ok(())`.
        timeout(Duration::from_secs(1), handle)
            .await
            .expect("watched connection did not exit when coordinator was dropped")
            .unwrap()
            .unwrap();

        // `graceful_shutdown` should NOT have been called on the force-close path.
        assert!(shutdown_called.await.is_err());
    }

    #[tokio::test]
    async fn dropping_serve_future_force_closes_connection() {
        let listener = crate::inmemory::InMemoryListener::new();
        let server = Server::builder().build();

        // A never-resolving signal future (we won't signal gracefully, we will drop the serve future directly).
        let (_signal_tx, signal_rx) = tokio::sync::oneshot::channel::<()>();

        let listener_clone = listener.clone();
        let (serve_started_tx, serve_started_rx) = tokio::sync::oneshot::channel::<()>();

        // Run `serve_with_shutdown` inside a task we can abort/drop (or wrap with timeout).
        let serve_handle = tokio::spawn(async move {
            let serve_fut = server.serve_with_shutdown(listener_clone, async {
                let _ = signal_rx.await;
            });
            let _ = serve_started_tx.send(());
            // Wrap in a short timeout so the future drops mid-flight while connections are active.
            let _ = tokio::time::timeout(std::time::Duration::from_millis(50), serve_fut).await;
        });

        serve_started_rx.await.unwrap();

        // When the 50ms timeout in `serve_handle` elapses, `serve_fut` is dropped -> `coordinator.tx` dropped -> connection self-terminates.
        serve_handle.await.unwrap();
    }

    #[tokio::test]
    async fn infinite_connection_graceful_shutdown_then_drop() {
        let coordinator = GracefulCoordinator::new();
        // `_finish` is never sent, so the connection never finishes.
        let (conn, shutdown_called, _finish) = MockConnection::new();

        let watched = coordinator.watch(conn);
        let handle = tokio::spawn(watched);

        tokio::task::yield_now().await;

        // 1. Initiate graceful shutdown. `tx.send(())` wakes `rx.changed()`, triggering `graceful_shutdown()`.
        // But because `conn` never completes, `coordinator.shutdown()` would hang if awaited.
        let _ = coordinator.tx.send(());
        shutdown_called.await.unwrap();
        assert!(!handle.is_finished());

        // 2. Escalate to forceful shutdown by dropping the coordinator (simulating dropping the serve future).
        drop(coordinator);

        // The watched task MUST self-terminate (`rx.changed()` returns `Err`, so the watcher returns `Ok(())`) despite being mid-drain.
        timeout(Duration::from_secs(1), handle)
            .await
            .expect("infinite watched connection did not exit when coordinator was dropped after graceful shutdown")
            .unwrap()
            .unwrap();
    }

    #[derive(Debug)]
    struct MockListenerAddress;

    impl ListenerAddress for MockListenerAddress {
        fn network(&self) -> &str {
            "mock"
        }
    }

    /// A mock listener that records when it is dropped via an atomic flag,
    /// and yields [`MockTransport`]s from a channel.
    struct MockListener {
        dropped: Arc<AtomicBool>,
        rx: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<MockTransport>>,
    }

    impl MockListener {
        fn new() -> (
            Self,
            Arc<AtomicBool>,
            tokio::sync::mpsc::UnboundedSender<MockTransport>,
        ) {
            let dropped = Arc::new(AtomicBool::new(false));
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            (
                Self {
                    dropped: dropped.clone(),
                    rx: tokio::sync::Mutex::new(rx),
                },
                dropped,
                tx,
            )
        }
    }

    impl Drop for MockListener {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    impl super::sealed::Sealed for MockListener {}

    impl Listener for MockListener {
        type Transport = MockTransport;

        async fn accept(&self, _token: Internal) -> Result<Self::Transport, String> {
            self.rx
                .lock()
                .await
                .recv()
                .await
                .ok_or_else(|| "mock listener closed".to_string())
        }

        fn local_addr(&self) -> Box<dyn ListenerAddress> {
            Box::new(MockListenerAddress)
        }
    }

    struct NopSendStream;

    impl SendStream for NopSendStream {
        async fn send(
            &mut self,
            _item: ResponseStreamItem<'_>,
            _options: SendOptions,
        ) -> Result<(), ()> {
            Ok(())
        }
    }

    struct NopRecvStream;

    impl RecvStream for NopRecvStream {
        async fn next(&mut self, _msg: &mut dyn RecvMessage) -> Option<Result<(), ()>> {
            None
        }
    }

    struct MockServingConnection {
        inner: Pin<Box<dyn Future<Output = Result<(), String>> + Send>>,
    }

    impl Future for MockServingConnection {
        type Output = Result<(), String>;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), String>> {
            self.inner.as_mut().poll(cx)
        }
    }

    impl GracefulConnection for MockServingConnection {
        fn graceful_shutdown(self: Pin<&mut Self>, _token: crate::private::Internal) {
            // No-op for mock connection
        }
    }

    struct MockTransport;

    impl Transport for MockTransport {
        type Connection = MockServingConnection;

        fn serve(
            self,
            handler: Arc<dyn DynHandle>,
            _runtime: GrpcRuntime,
            _token: crate::private::Internal,
        ) -> Self::Connection {
            let inner = Box::pin(async move {
                let mut tx = NopSendStream;
                let rx = Box::new(NopRecvStream);
                let _ = handler
                    .dyn_handle(
                        RequestHeaders::new("/test.Draining/Method", test_connection_info()),
                        CallOptions::new(),
                        &mut tx,
                        rx,
                    )
                    .await;
                Ok(())
            });
            MockServingConnection { inner }
        }
    }

    #[tokio::test]
    async fn listener_dropped_when_shutdown_signal_fires() {
        let (listener, dropped, _tx) = MockListener::new();

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let server_handle = tokio::spawn(async move {
            Server::builder()
                .build()
                .serve_with_shutdown(listener, async {
                    let _ = shutdown_rx.await;
                })
                .await
                .unwrap();
        });

        tokio::task::yield_now().await;
        assert!(!dropped.load(Ordering::SeqCst));

        // Fire shutdown immediately.
        let _ = shutdown_tx.send(());

        // Server should exit promptly and drop the listener.
        tokio::time::timeout(std::time::Duration::from_secs(2), server_handle)
            .await
            .expect("server did not shut down within 2s")
            .unwrap();

        assert!(
            dropped.load(Ordering::SeqCst),
            "listener should be dropped after serve_with_shutdown completes"
        );
    }

    #[tokio::test]
    async fn listener_dropped_immediately_while_connections_drain() {
        use crate::server::RecvStream;
        use crate::server::RequestHeaders;
        use crate::server::SendStream;
        use crate::server::Trailers;
        use crate::server::descriptor::MethodDescriptor;
        use crate::server::descriptor::ServiceDescriptor;
        use crate::server::service::Service;

        let (listener, dropped, tx) = MockListener::new();

        let handler_started = Arc::new(AtomicBool::new(false));
        let (unblock_tx, unblock_rx) = tokio::sync::oneshot::channel::<()>();
        let unblock_rx = Arc::new(tokio::sync::Mutex::new(Some(unblock_rx)));

        struct DrainingHandler {
            started: Arc<AtomicBool>,
            unblock: Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>,
        }

        impl Handle for DrainingHandler {
            async fn handle(
                &self,
                _: RequestHeaders,
                _: CallOptions,
                _: &mut impl SendStream,
                _: impl RecvStream + 'static,
            ) -> Trailers {
                self.started.store(true, Ordering::SeqCst);
                if let Some(rx) = self.unblock.lock().await.take() {
                    let _ = rx.await;
                }
                Trailers::new(Ok(()))
            }
        }

        struct DrainingService {
            started: Arc<AtomicBool>,
            unblock: Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>,
        }

        impl Service for DrainingService {
            fn descriptor(&self) -> ServiceDescriptor {
                ServiceDescriptor::new(
                    "test.Draining",
                    vec![MethodDescriptor::new("/test.Draining/Method")],
                )
            }

            fn register_methods(self) -> Vec<(String, Arc<dyn DynHandle>)> {
                vec![(
                    "/test.Draining/Method".to_string(),
                    Arc::new(DrainingHandler {
                        started: self.started,
                        unblock: self.unblock,
                    }),
                )]
            }
        }

        let server = Server::builder()
            .add_service(DrainingService {
                started: handler_started.clone(),
                unblock: unblock_rx,
            })
            .build();

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let server_handle = tokio::spawn(async move {
            server
                .serve_with_shutdown(listener, async {
                    let _ = shutdown_rx.await;
                })
                .await
                .unwrap();
        });

        // Send a MockTransport to the listener so the server accepts a connection.
        tx.send(MockTransport)
            .expect("should send mock transport to listener");

        // Wait until handler has started running.
        for _ in 0..50 {
            if handler_started.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(handler_started.load(Ordering::SeqCst));

        // Listener is still alive before shutdown signal.
        assert!(!dropped.load(Ordering::SeqCst));

        // Fire shutdown signal.
        let _ = shutdown_tx.send(());

        // Wait brief moment for select! loop to break and drop(listener) to execute.
        for _ in 0..50 {
            if dropped.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        // 1. The listener must be dropped immediately, even though the RPC connection is still draining.
        assert!(
            dropped.load(Ordering::SeqCst),
            "listener should be dropped immediately when shutdown signal fires"
        );

        // 2. New incoming connections should be rejected immediately.
        assert!(
            tx.send(MockTransport).is_err(),
            "new connections must be rejected after listener is dropped"
        );

        // 3. The server future is still running because existing connection is draining (handler waiting on unblock_rx).
        assert!(
            !server_handle.is_finished(),
            "server should wait for existing connections to drain"
        );

        // Unblock the handler so the RPC finishes and graceful shutdown completes.
        let _ = unblock_tx.send(());

        tokio::time::timeout(std::time::Duration::from_secs(2), server_handle)
            .await
            .expect("server did not shut down after connection drained")
            .unwrap();
    }

    #[tokio::test]
    async fn server_serve_propagates_accept_error() {
        struct FailingListener;

        impl sealed::Sealed for FailingListener {}

        impl Listener for FailingListener {
            type Transport = MockTransport;

            async fn accept(&self, _token: Internal) -> Result<Self::Transport, String> {
                Err("simulated listener accept error".to_string())
            }

            fn local_addr(&self) -> Box<dyn ListenerAddress> {
                Box::new(MockListenerAddress)
            }
        }

        let server = Server::builder().build();
        let result = server.serve(FailingListener).await;
        assert_eq!(
            result,
            Err("simulated listener accept error".to_string()),
            "server::serve must propagate accept error to the caller"
        );
    }

    #[tokio::test]
    async fn tcp_listener_binds_and_returns_local_tcp_address() {
        let creds: Arc<dyn ServerCredentials> = Arc::new(LocalServerCredentials::new());
        let config = Http2Config::new().with_max_recv_message_size(1024);

        let listener = Server::tcp_listener("127.0.0.1:0".parse().unwrap(), creds, config)
            .await
            .unwrap();

        let local_addr = listener.local_addr();
        let tcp_addr = (&*local_addr as &dyn Any)
            .downcast_ref::<TcpAddress>()
            .expect("expected TcpAddress");
        assert!(tcp_addr.0.port() > 0);
    }
}
