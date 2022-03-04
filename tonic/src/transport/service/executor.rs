use futures_core::future::BoxFuture;
use std::{future::Future, sync::Arc};

/// An executor of futures.
pub trait Executor<Fut> {
    /// Place the future into the executor to be run.
    fn execute(&self, fut: Fut);
}

#[derive(Copy, Clone)]
struct TokioExec;

impl<F> Executor<F> for TokioExec
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn execute(&self, fut: F) {
        tokio::spawn(fut);
    }
}

#[derive(Clone)]
pub(crate) struct SharedExec {
    inner: Arc<dyn Executor<BoxFuture<'static, ()>> + Send + Sync + 'static>,
}

impl SharedExec {
    pub(crate) fn new<E>(exec: E) -> Self
    where
        E: Executor<BoxFuture<'static, ()>> + Send + Sync + 'static,
    {
        Self {
            inner: Arc::new(exec),
        }
    }

    pub(crate) fn tokio() -> Self {
        Self::new(TokioExec)
    }
}

impl Executor<BoxFuture<'static, ()>> for SharedExec {
    fn execute(&self, fut: BoxFuture<'static, ()>) {
        self.inner.execute(fut)
    }
}

impl hyper::rt::Executor<BoxFuture<'static, ()>> for SharedExec {
    fn execute(&self, fut: BoxFuture<'static, ()>) {
        Executor::execute(self, fut);
    }
}
