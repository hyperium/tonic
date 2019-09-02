use tower_make::MakeService;
use tower_service::Service;

#[derive(Debug)]
pub struct Reconnect<M> {
    inner: M,
}

impl<M, Target, Request> Service<Target> for Reconnect<M>
where
    M: MakeService<Target, Request>,
{
    type Response = M::Response;
    type Error = M::Error;
    type Future = M::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Target) -> Self::Future {
        unimplmented!()
    }
}
