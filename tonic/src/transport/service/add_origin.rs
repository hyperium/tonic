use http::{Request, Uri};
use std::task::{Context, Poll};
use tower_service::Service;

#[derive(Debug)]
pub(crate) struct AddOrigin<T> {
    inner: T,
    origin: Uri,
}

impl<T> AddOrigin<T> {
    pub(crate) fn new(inner: T, origin: Uri) -> Self {
        Self { inner, origin }
    }
}

impl<T, ReqBody> Service<Request<ReqBody>> for AddOrigin<T>
where
    T: Service<Request<ReqBody>>,
{
    type Response = T::Response;
    type Error = T::Error;
    type Future = T::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        // Split the request into the head and the body.
        let (mut head, body) = req.into_parts();

        // Split the request URI into parts.
        let mut uri: http::uri::Parts = head.uri.into();
        let set_uri = self.origin.clone().into_parts();

        // Update the URI parts, setting hte scheme and authority
        uri.scheme = Some(set_uri.scheme.expect("expected scheme").clone());
        uri.authority = Some(set_uri.authority.expect("expected authority").clone());

        // Update the the request URI
        head.uri = http::Uri::from_parts(uri).expect("valid uri");

        let request = Request::from_parts(head, body);

        self.inner.call(request)
    }
}
