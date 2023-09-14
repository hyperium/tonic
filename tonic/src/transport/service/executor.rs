use crate::transport::BoxFuture;
use std::{future::Future, sync::Arc};

pub(crate) use hyper::rt::Executor;

#[derive(Default, Debug, Copy, Clone)]
pub struct TokioExec;

impl<F> Executor<F> for TokioExec
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn execute(&self, fut: F) {
        tokio::spawn(fut);
    }
}

#[derive(Default, Debug, Copy, Clone)]
pub struct LocalExec;

impl<F> Executor<F> for LocalExec
where
    F: Future + 'static,
    F::Output: 'static,
{
    fn execute(&self, fut: F) {
        tokio::task::spawn_local(fut);
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
