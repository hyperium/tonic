// From https://github.com/hyperium/hyper-util/blob/7afb1ed5337c0689d7341e09d31578f1fcffc8af/src/server/graceful.rs,
// implements Clone for GracefulShutdown.

use std::{
    fmt::{self, Debug},
    future::Future,
    pin::Pin,
    task::{self, Poll},
};

use pin_project::pin_project;
use tokio::sync::watch;

/// A graceful shutdown utility
#[derive(Clone)]
pub(super) struct GracefulShutdown {
    tx: watch::Sender<()>,
}

impl GracefulShutdown {
    /// Create a new graceful shutdown helper.
    pub(super) fn new() -> Self {
        let (tx, _) = watch::channel(());
        Self { tx }
    }

    /// Wrap a future for graceful shutdown watching.
    pub(super) fn watch<C: GracefulConnection>(&self, conn: C) -> impl Future<Output = C::Output> {
        let mut rx = self.tx.subscribe();
        GracefulConnectionFuture::new(conn, async move {
            let _ = rx.changed().await;
            // hold onto the rx until the watched future is completed
            rx
        })
    }

    /// Signal shutdown for all watched connections.
    ///
    /// This returns a `Future` which will complete once all watched
    /// connections have shutdown.
    pub(super) async fn shutdown(self) {
        let Self { tx } = self;

        // signal all the watched futures about the change
        let _ = tx.send(());
        // and then wait for all of them to complete
        tx.closed().await;
    }
}

impl Debug for GracefulShutdown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GracefulShutdown").finish()
    }
}

impl Default for GracefulShutdown {
    fn default() -> Self {
        Self::new()
    }
}

#[pin_project]
struct GracefulConnectionFuture<C, F: Future> {
    #[pin]
    conn: C,
    #[pin]
    cancel: F,
    #[pin]
    // If cancelled, this is held until the inner conn is done.
    cancelled_guard: Option<F::Output>,
}

impl<C, F: Future> GracefulConnectionFuture<C, F> {
    fn new(conn: C, cancel: F) -> Self {
        Self {
            conn,
            cancel,
            cancelled_guard: None,
        }
    }
}

impl<C, F: Future> Debug for GracefulConnectionFuture<C, F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GracefulConnectionFuture").finish()
    }
}

impl<C, F> Future for GracefulConnectionFuture<C, F>
where
    C: GracefulConnection,
    F: Future,
{
    type Output = C::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();
        if this.cancelled_guard.is_none() {
            if let Poll::Ready(guard) = this.cancel.poll(cx) {
                this.cancelled_guard.set(Some(guard));
                this.conn.as_mut().graceful_shutdown();
            }
        }
        this.conn.poll(cx)
    }
}

/// An internal utility trait as an umbrella target for all (hyper) connection
/// types that the [`GracefulShutdown`] can watch.
pub(super) trait GracefulConnection:
    Future<Output = Result<(), Self::Error>> + private::Sealed
{
    /// The error type returned by the connection when used as a future.
    type Error;

    /// Start a graceful shutdown process for this connection.
    fn graceful_shutdown(self: Pin<&mut Self>);
}

impl<I, B, S> GracefulConnection for hyper::server::conn::http1::Connection<I, S>
where
    S: hyper::service::HttpService<hyper::body::Incoming, ResBody = B>,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    B: hyper::body::Body + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    type Error = hyper::Error;

    fn graceful_shutdown(self: Pin<&mut Self>) {
        hyper::server::conn::http1::Connection::graceful_shutdown(self);
    }
}

impl<I, B, S, E> GracefulConnection for hyper::server::conn::http2::Connection<I, S, E>
where
    S: hyper::service::HttpService<hyper::body::Incoming, ResBody = B>,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    B: hyper::body::Body + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    E: hyper::rt::bounds::Http2ServerConnExec<S::Future, B>,
{
    type Error = hyper::Error;

    fn graceful_shutdown(self: Pin<&mut Self>) {
        hyper::server::conn::http2::Connection::graceful_shutdown(self);
    }
}

impl<'a, I, B, S, E> GracefulConnection for hyper_util::server::conn::auto::Connection<'a, I, S, E>
where
    S: hyper::service::Service<http::Request<hyper::body::Incoming>, Response = http::Response<B>>,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    S::Future: 'static,
    I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    B: hyper::body::Body + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    E: hyper::rt::bounds::Http2ServerConnExec<S::Future, B>,
{
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn graceful_shutdown(self: Pin<&mut Self>) {
        hyper_util::server::conn::auto::Connection::graceful_shutdown(self);
    }
}

impl<'a, I, B, S, E> GracefulConnection
    for hyper_util::server::conn::auto::UpgradeableConnection<'a, I, S, E>
where
    S: hyper::service::Service<http::Request<hyper::body::Incoming>, Response = http::Response<B>>,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    S::Future: 'static,
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
    B: hyper::body::Body + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    E: hyper::rt::bounds::Http2ServerConnExec<S::Future, B>,
{
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn graceful_shutdown(self: Pin<&mut Self>) {
        hyper_util::server::conn::auto::UpgradeableConnection::graceful_shutdown(self);
    }
}

mod private {
    pub(crate) trait Sealed {}

    impl<I, B, S> Sealed for hyper::server::conn::http1::Connection<I, S>
    where
        S: hyper::service::HttpService<hyper::body::Incoming, ResBody = B>,
        S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
        B: hyper::body::Body + 'static,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
    }

    impl<I, B, S> Sealed for hyper::server::conn::http1::UpgradeableConnection<I, S>
    where
        S: hyper::service::HttpService<hyper::body::Incoming, ResBody = B>,
        S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
        B: hyper::body::Body + 'static,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
    }

    impl<I, B, S, E> Sealed for hyper::server::conn::http2::Connection<I, S, E>
    where
        S: hyper::service::HttpService<hyper::body::Incoming, ResBody = B>,
        S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
        B: hyper::body::Body + 'static,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        E: hyper::rt::bounds::Http2ServerConnExec<S::Future, B>,
    {
    }

    impl<'a, I, B, S, E> Sealed for hyper_util::server::conn::auto::Connection<'a, I, S, E>
    where
        S: hyper::service::Service<
            http::Request<hyper::body::Incoming>,
            Response = http::Response<B>,
        >,
        S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        S::Future: 'static,
        I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
        B: hyper::body::Body + 'static,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        E: hyper::rt::bounds::Http2ServerConnExec<S::Future, B>,
    {
    }

    impl<'a, I, B, S, E> Sealed for hyper_util::server::conn::auto::UpgradeableConnection<'a, I, S, E>
    where
        S: hyper::service::Service<
            http::Request<hyper::body::Incoming>,
            Response = http::Response<B>,
        >,
        S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        S::Future: 'static,
        I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
        B: hyper::body::Body + 'static,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        E: hyper::rt::bounds::Http2ServerConnExec<S::Future, B>,
    {
    }
}
