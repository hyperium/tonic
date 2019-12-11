use pin_project::pin_project;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

#[pin_project]
pub struct ResponseFuture<F> {
    inner: F,
    span: Option<Span>,
}

impl<F: Future> Future for ResponseFuture<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let me = self.project();

        if let Some(span) = me.span.clone().take() {
            let _enter = span.enter();
            // me.poll(cx).map_err(Into::into)
        }
    }
}
