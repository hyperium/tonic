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

use pin_project::pin_project;
use std::fmt;
use std::time::Duration;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tower::make::MakeService;
use tower_service::Service;
use tracing::trace;

pub(crate) const DEFAULT_RECONNECT_DELAY: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReconnectMode {
    Eager,
    Lazy,
    Balanced,
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
    has_been_connected: bool,
    mode: ReconnectMode,
    reconnect_delay: Duration,
}

#[derive(Debug)]
enum State<F, S> {
    Idle,
    Connecting(F),
    Connected(S),
    Backoff(Pin<Box<tokio::time::Sleep>>),
}

impl<M, Target> Reconnect<M, Target>
where
    M: Service<Target>,
    M::Error: Into<crate::BoxError>,
{
    pub(crate) fn new(
        mk_service: M,
        target: Target,
        mode: ReconnectMode,
        reconnect_delay: Option<Duration>,
    ) -> Self {
        Reconnect {
            mk_service,
            state: State::Idle,
            target,
            error: None,
            has_been_connected: false,
            mode,
            reconnect_delay: reconnect_delay.unwrap_or(DEFAULT_RECONNECT_DELAY),
        }
    }
}

impl<M, Target, S, Request> Service<Request> for Reconnect<M, Target>
where
    M: Service<Target, Response = S>,
    S: Service<Request>,
    M::Future: Unpin,
    crate::BoxError: From<M::Error> + From<S::Error>,
    Target: Clone,
    <M as tower_service::Service<Target>>::Error: Into<crate::BoxError>,
{
    type Response = S::Response;
    type Error = crate::BoxError;
    type Future = ResponseFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let mut state;

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
                            state = State::Connected(service);
                        }
                        Poll::Pending => {
                            trace!("poll_ready; not ready");
                            return Poll::Pending;
                        }
                        Poll::Ready(Err(e)) => {
                            trace!("poll_ready; error");

                            match self.mode {
                                ReconnectMode::Eager => {
                                    self.state = State::Idle;
                                    return Poll::Ready(Err(e.into()));
                                }
                                ReconnectMode::Lazy => {
                                    state = State::Idle;
                                    let error = e.into();
                                    tracing::debug!("reconnect::poll_ready: {:?}", error);
                                    self.error = Some(error);
                                    break;
                                }
                                ReconnectMode::Balanced => {
                                    trace!("poll_ready; balanced backoff");
                                    self.state = State::Backoff(Box::pin(tokio::time::sleep(
                                        self.reconnect_delay,
                                    )));
                                    continue;
                                }
                            }
                        }
                    }
                }
                State::Connected(ref mut inner) => {
                    trace!("poll_ready; connected");

                    self.has_been_connected = true;

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
                            match self.mode {
                                ReconnectMode::Balanced => {
                                    self.state = State::Backoff(Box::pin(tokio::time::sleep(
                                        self.reconnect_delay,
                                    )));
                                    continue;
                                }
                                _ => {
                                    state = State::Idle;
                                }
                            }
                        }
                    }
                }
                State::Backoff(ref mut sleep) => {
                    trace!("poll_ready; backoff");
                    match Pin::new(sleep).poll(cx) {
                        Poll::Pending => {
                            trace!("poll_ready; backoff pending");
                            return Poll::Pending;
                        }
                        Poll::Ready(()) => {
                            trace!("poll_ready; backoff elapsed");
                            self.state = State::Idle;
                            continue;
                        }
                    }
                }
            }

            self.state = state;
        }

        self.state = state;
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
        ResponseFuture::new(fut)
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
}

#[pin_project(project = InnerProj)]
#[derive(Debug)]
enum Inner<F> {
    Future(#[pin] F),
    Error(Option<crate::BoxError>),
}

impl<F> ResponseFuture<F> {
    pub(crate) fn new(inner: F) -> Self {
        ResponseFuture {
            inner: Inner::Future(inner),
        }
    }

    pub(crate) fn error(error: crate::BoxError) -> Self {
        ResponseFuture {
            inner: Inner::Error(Some(error)),
        }
    }
}

impl<F, T, E> Future for ResponseFuture<F>
where
    F: Future<Output = Result<T, E>>,
    E: Into<crate::BoxError>,
{
    type Output = Result<T, crate::BoxError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        //self.project().inner.poll(cx).map_err(Into::into)
        let me = self.project();
        match me.inner.project() {
            InnerProj::Future(fut) => fut.poll(cx).map_err(Into::into),
            InnerProj::Error(e) => {
                let e = e.take().expect("Polled after ready.");
                Poll::Ready(Err(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tower::service_fn;

    #[derive(Clone)]
    struct FailingMakeService {
        attempts: Arc<AtomicUsize>,
        fail_times: usize,
    }

    impl Service<()> for FailingMakeService {
        type Response = tower::util::BoxService<(), (), crate::BoxError>;
        type Error = crate::BoxError;
        type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _: ()) -> Self::Future {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
            if attempt < self.fail_times {
                std::future::ready(Err("connection failed".into()))
            } else {
                std::future::ready(Ok(tower::util::BoxService::new(service_fn(|()| async {
                    Ok(())
                }))))
            }
        }
    }

    #[tokio::test]
    async fn test_reconnect_mode_eager() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let mk_svc = FailingMakeService {
            attempts: attempts.clone(),
            fail_times: 1,
        };

        let mut reconnect = Reconnect::new(mk_svc, (), ReconnectMode::Eager, None);

        let poll = std::future::poll_fn(|cx| reconnect.poll_ready(cx)).await;
        assert!(
            poll.is_err(),
            "Eager mode should return Err on connect failure"
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_reconnect_mode_lazy() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let mk_svc = FailingMakeService {
            attempts: attempts.clone(),
            fail_times: 1,
        };

        let mut reconnect = Reconnect::new(mk_svc, (), ReconnectMode::Lazy, None);

        let poll = std::future::poll_fn(|cx| reconnect.poll_ready(cx)).await;
        assert!(
            poll.is_ok(),
            "Lazy mode should return Ready(Ok) on connect failure"
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        let res = reconnect.call(()).await;
        assert!(res.is_err(), "Lazy mode should return error from call");

        // Subsequent poll should reconnect and succeed
        let poll2 = std::future::poll_fn(|cx| reconnect.poll_ready(cx)).await;
        assert!(poll2.is_ok());
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_reconnect_mode_balanced_backs_off() {
        tokio::time::pause();

        let attempts = Arc::new(AtomicUsize::new(0));
        let mk_svc = FailingMakeService {
            attempts: attempts.clone(),
            fail_times: 2,
        };

        let delay = Duration::from_millis(100);
        let mut reconnect = Reconnect::new(mk_svc, (), ReconnectMode::Balanced, Some(delay));

        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);

        let p1 = reconnect.poll_ready(&mut cx);
        assert!(
            p1.is_pending(),
            "Balanced mode must return Pending on connect failure"
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        // Advance time partially (50ms): should still be pending
        tokio::time::advance(Duration::from_millis(50)).await;
        let p2 = reconnect.poll_ready(&mut cx);
        assert!(p2.is_pending());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        // Advance past delay: next poll attempts and fails again
        tokio::time::advance(Duration::from_millis(60)).await;
        let p3 = reconnect.poll_ready(&mut cx);
        assert!(p3.is_pending());
        assert_eq!(attempts.load(Ordering::SeqCst), 2);

        // Advance past second delay: next poll attempts and succeeds
        tokio::time::advance(Duration::from_millis(110)).await;
        let p4 = reconnect.poll_ready(&mut cx);
        assert!(
            matches!(p4, Poll::Ready(Ok(()))),
            "Balanced mode should return Ready(Ok) once connection succeeds"
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }
}
