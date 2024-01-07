use crate::transport::BoxFuture;
use std::{future::Future, sync::Arc};

pub(crate) use hyper::rt::Executor;

#[cfg(not(target_arch = "wasm32"))]
#[derive(Copy, Clone)]
struct TokioExec;

#[cfg(not(target_arch = "wasm32"))]
impl<F> Executor<F> for TokioExec
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn execute(&self, fut: F) {
        tokio::spawn(fut);
    }
}

#[cfg(target_arch = "wasm32")]
#[derive(Copy, Clone)]
struct WasmBindgenExec;

#[cfg(target_arch = "wasm32")]
impl<F> Executor<F> for WasmBindgenExec
where
    F: Future + 'static,
    F::Output: 'static,
{
    fn execute(&self, fut: F) {
        wasm_bindgen_futures::spawn_local(async move {fut.await;});
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

    pub(crate) fn default_exec() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        return Self::new(TokioExec);
        #[cfg(target_arch = "wasm32")]
        Self::new(WasmBindgenExec)
    }
}

impl Executor<BoxFuture<'static, ()>> for SharedExec {
    fn execute(&self, fut: BoxFuture<'static, ()>) {
        self.inner.execute(fut)
    }
}
