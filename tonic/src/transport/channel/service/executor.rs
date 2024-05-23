use crate::transport::BoxFuture;
use std::{future::Future, sync::Arc};

pub(crate) use hyper::rt::Executor;

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

impl<F> Executor<F> for SharedExec
where
    F: Future<Output = ()> + Send + 'static,
{
    fn execute(&self, fut: F) {
        self.inner.execute(Box::pin(fut))
    }
}
