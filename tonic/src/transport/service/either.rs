use futures_util::future::{MapErr, TryFutureExt};
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tower::Service;

pub(crate) enum Either<A, B> {
    A(A),
    B(B),
}

impl<A, B, Request, Response> Service<Request> for Either<A, B>
where
    A: Service<Request, Response = Response>,
    B: Service<Request, Response = Response>,
    A::Error: Into<crate::Error>,
    B::Error: Into<crate::Error>,
{
    type Response = Response;
    type Error = crate::Error;
    type Future = Either<
        MapErr<A::Future, fn(A::Error) -> crate::Error>,
        MapErr<B::Future, fn(B::Error) -> crate::Error>,
    >;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self {
            Either::A(svc) => svc.poll_ready(cx).map_err(Into::into),
            Either::B(svc) => svc.poll_ready(cx).map_err(Into::into),
        }
    }

    fn call(&mut self, req: Request) -> Self::Future {
        match self {
            Either::A(svc) => {
                let fut = svc
                    .call(req)
                    .map_err((|e| e.into()) as fn(A::Error) -> crate::Error);
                Either::A(fut)
            }

            Either::B(svc) => {
                let fut = svc
                    .call(req)
                    .map_err((|e| e.into()) as fn(B::Error) -> crate::Error);
                Either::B(fut)
            }
        }
    }
}

impl<A: Unpin, B: Unpin> Unpin for Either<A, B> {}

impl<A, B> Future for Either<A, B>
where
    A: Future,
    B: Future<Output = A::Output>,
{
    type Output = A::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // safe because we do not exposed an unchecked mut beyond this projection.
        let mut me = unsafe { self.get_unchecked_mut() };

        match &mut me {
            Either::A(fut) => unsafe { Pin::new_unchecked(fut) }.poll(cx),
            Either::B(fut) => unsafe { Pin::new_unchecked(fut) }.poll(cx),
        }
    }
}
