use crate::Error;
use pin_project::pin_project;
use std::fmt;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tower_make::MakeService;
use tower_service::Service;
use tracing::trace;

pub(crate) struct Reconnect<M, Target>
where
    M: Service<Target>,
{
    mk_service: M,
    state: State<M::Future, M::Response>,
    target: Target,
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
{
    pub(crate) fn new<S, Request>(initial_connection: S, mk_service: M, target: Target) -> Self
    where
        M: Service<Target, Response = S>,
        S: Service<Request>,
        Error: From<M::Error> + From<S::Error>,
        Target: Clone,
    {
        Reconnect {
            mk_service,
            state: State::Connected(initial_connection),
            target,
        }
    }
}

impl<M, Target, S, Request> Service<Request> for Reconnect<M, Target>
where
    M: Service<Target, Response = S>,
    S: Service<Request>,
    M::Future: Unpin,
    Error: From<M::Error> + From<S::Error>,
    Target: Clone,
{
    type Response = S::Response;
    type Error = Error;
    type Future = ResponseFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let ret;
        let mut state;

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
                            state = State::Connected(service);
                        }
                        Poll::Pending => {
                            trace!("poll_ready; not ready");
                            return Poll::Pending;
                        }
                        Poll::Ready(Err(e)) => {
                            trace!("poll_ready; error");
                            state = State::Idle;
                            ret = Err(e.into());
                            break;
                        }
                    }
                }
                State::Connected(ref mut inner) => {
                    trace!("poll_ready; connected");
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
                            state = State::Idle;
                        }
                    }
                }
            }

            self.state = state;
        }

        self.state = state;
        Poll::Ready(ret)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let service = match self.state {
            State::Connected(ref mut service) => service,
            _ => panic!("service not ready; poll_ready must be called first"),
        };

        let fut = service.call(request);
        ResponseFuture::new(fut)
    }
}

impl<M, Target> fmt::Debug for Reconnect<M, Target>
where
    M: Service<Target> + fmt::Debug,
    M::Future: fmt::Debug,
    M::Response: fmt::Debug,
    Target: fmt::Debug,
{
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("Reconnect")
            .field("mk_service", &self.mk_service)
            .field("state", &self.state)
            .field("target", &self.target)
            .finish()
    }
}

#[pin_project]
#[derive(Debug)]
pub(crate) struct ResponseFuture<F> {
    #[pin]
    inner: F,
}

impl<F> ResponseFuture<F> {
    pub(crate) fn new(inner: F) -> Self {
        ResponseFuture { inner }
    }
}

impl<F, T, E> Future for ResponseFuture<F>
where
    F: Future<Output = Result<T, E>>,
    E: Into<Error>,
{
    type Output = Result<T, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.project().inner.poll(cx).map_err(Into::into)
    }
}
