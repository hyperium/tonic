use pin_project::pin_project;
use std::fmt;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::task::ready;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tower::make::MakeService;
use tower_service::Service;
use tracing::trace;

use crate::body::Body;

/// Allows request responses to require a reconnect based on returned values
#[derive(Debug, Clone)]
pub(crate) struct ReconnectNotify(Arc<AtomicBool>);

impl ReconnectNotify {
    fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    fn reconnect(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Release)
    }

    fn reset(&self) {
        self.0.store(false, std::sync::atomic::Ordering::Release)
    }

    fn needs_reconnect(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }
}

pub(crate) struct Reconnect<M, Target>
where
    M: Service<Target>,
    M::Error: Into<crate::BoxError>,
{
    mk_service: M,
    state: State<M::Future, M::Response>,
    target: Target,
    error: Option<crate::BoxError>,
    needs_reconnect: ReconnectNotify,
    has_been_connected: bool,
    is_lazy: bool,
}

#[derive(Debug)]
enum State<F, S> {
    Idle,
    Connecting(F),
    Connected(S),
}

impl<M, Target> Reconnect<M, Target>
where
    M: Service<Target>,
    M::Error: Into<crate::BoxError>,
{
    pub(crate) fn new(mk_service: M, target: Target, is_lazy: bool) -> Self {
        Reconnect {
            mk_service,
            state: State::Idle,
            target,
            error: None,
            needs_reconnect: ReconnectNotify::new(),
            has_been_connected: false,
            is_lazy,
        }
    }
}

impl<M, Target, S, Request, E> Service<Request> for Reconnect<M, Target>
where
    M: Service<Target, Response = S>,
    S: Service<Request>,
    M::Future: Unpin,
    S::Future: Future<Output = Result<http::Response<Body>, E>>,
    crate::BoxError: From<M::Error> + From<S::Error> + From<E>,
    Target: Clone,
    <M as tower_service::Service<Target>>::Error: Into<crate::BoxError>,
{
    type Response = http::Response<Body>;
    type Error = crate::BoxError;
    type Future = ResponseFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.error.is_some() {
            return Poll::Ready(Ok(()));
        }

        loop {
            match self.state {
                State::Idle => {
                    trace!("poll_ready; idle");
                    match self.mk_service.poll_ready(cx) {
                        Poll::Ready(r) => r?,
                        Poll::Pending => {
                            trace!("poll_ready; MakeService not ready");
                            return Poll::Pending;
                        }
                    }

                    let fut = self.mk_service.make_service(self.target.clone());
                    self.state = State::Connecting(fut);
                    continue;
                }
                State::Connecting(ref mut f) => {
                    trace!("poll_ready; connecting");
                    match Pin::new(f).poll(cx) {
                        Poll::Ready(Ok(service)) => {
                            self.needs_reconnect.reset();
                            self.state = State::Connected(service);
                        }
                        Poll::Pending => {
                            trace!("poll_ready; not ready");
                            return Poll::Pending;
                        }
                        Poll::Ready(Err(e)) => {
                            trace!("poll_ready; error");

                            self.state = State::Idle;

                            if !(self.has_been_connected || self.is_lazy) {
                                return Poll::Ready(Err(e.into()));
                            } else {
                                let error = e.into();
                                tracing::debug!("reconnect::poll_ready: {:?}", error);
                                self.error = Some(error);
                                break;
                            }
                        }
                    }
                }
                State::Connected(ref mut inner) => {
                    trace!("poll_ready; connected");

                    self.has_been_connected = true;
                    if self.needs_reconnect.needs_reconnect() {
                        trace!("poll_ready: needs_reconnect");
                        self.state = State::Idle;
                        continue;
                    }

                    match inner.poll_ready(cx) {
                        Poll::Ready(Ok(())) => {
                            trace!("poll_ready; ready");
                            return Poll::Ready(Ok(()));
                        }
                        Poll::Pending => {
                            trace!("poll_ready; not ready");
                            return Poll::Pending;
                        }
                        Poll::Ready(Err(_)) => {
                            trace!("poll_ready; error");
                            self.state = State::Idle;
                        }
                    }
                }
            }
        }

        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        tracing::trace!("Reconnect::call");
        if let Some(error) = self.error.take() {
            tracing::debug!("error: {}", error);
            return ResponseFuture::error(error);
        }

        let State::Connected(service) = &mut self.state else {
            panic!("service not ready; poll_ready must be called first");
        };

        let fut = service.call(request);
        ResponseFuture::new(fut, self.needs_reconnect.clone())
    }
}

impl<M, Target> fmt::Debug for Reconnect<M, Target>
where
    M: Service<Target> + fmt::Debug,
    M::Future: fmt::Debug,
    M::Response: fmt::Debug,
    Target: fmt::Debug,
    <M as tower_service::Service<Target>>::Error: Into<crate::BoxError>,
{
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("Reconnect")
            .field("mk_service", &self.mk_service)
            .field("state", &self.state)
            .field("target", &self.target)
            .finish()
    }
}

/// Future that resolves to the response or failure to connect.
#[pin_project]
#[derive(Debug)]
pub(crate) struct ResponseFuture<F> {
    #[pin]
    inner: Inner<F>,
    reconnect: ReconnectNotify,
}

#[pin_project(project = InnerProj)]
#[derive(Debug)]
enum Inner<F> {
    Future(#[pin] F),
    Error(Option<crate::BoxError>),
}

impl<F> ResponseFuture<F> {
    pub(crate) fn new(inner: F, reconnect: ReconnectNotify) -> Self {
        ResponseFuture {
            inner: Inner::Future(inner),
            reconnect,
        }
    }

    pub(crate) fn error(error: crate::BoxError) -> Self {
        ResponseFuture {
            inner: Inner::Error(Some(error)),
            reconnect: ReconnectNotify::new(),
        }
    }
}

impl<F, E> Future for ResponseFuture<F>
where
    F: Future<Output = Result<http::Response<Body>, E>>,
    E: Into<crate::BoxError>,
{
    type Output = Result<http::Response<Body>, crate::BoxError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let me = self.project();
        match me.inner.project() {
            InnerProj::Future(fut) => match ready!(fut.poll(cx)) {
                Ok(res) => {
                    // Server errors map to `Status::UNAVAILABLE` and are unrecoverable
                    if res.status().is_server_error() {
                        trace!("Reconnect: scheduled reconnect");
                        me.reconnect.reconnect();
                    }
                    Poll::Ready(Ok(res))
                }
                Err(err) => Poll::Ready(Err(err.into())),
            },
            InnerProj::Error(e) => {
                let e = e.take().expect("Polled after ready.");
                Poll::Ready(Err(e))
            }
        }
    }
}
