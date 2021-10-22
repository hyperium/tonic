use futures_core::future::BoxFuture;
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
    T::Future: Send + 'static,
    T::Error: Into<crate::Error>,
{
    type Response = T::Response;
    type Error = crate::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        // Split the request into the head and the body.
        let (mut head, body) = req.into_parts();

        // Split the request URI into parts.
        let mut uri: http::uri::Parts = head.uri.into();
        let set_uri = self.origin.clone().into_parts();

        if set_uri.scheme.is_none() || set_uri.authority.is_none() {
            let err = crate::transport::Error::new_invalid_uri();
            return Box::pin(async move { Err::<Self::Response, _>(err.into()) });
        }

        // Update the URI parts, setting hte scheme and authority
        uri.scheme = Some(set_uri.scheme.expect("expected scheme"));
        uri.authority = Some(set_uri.authority.expect("expected authority"));

        // Update the the request URI
        head.uri = http::Uri::from_parts(uri).expect("valid uri");

        let request = Request::from_parts(head, body);

        let fut = self.inner.call(request);

        Box::pin(async move { fut.await.map_err(Into::into) })
    }
}
