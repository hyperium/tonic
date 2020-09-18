use http::{header::USER_AGENT, HeaderValue, Request};
use std::task::{Context, Poll};
use tower_service::Service;

const TONIC_USER_AGENT: &str = concat!("tonic/", env!("CARGO_PKG_VERSION"));

#[derive(Debug)]
pub(crate) struct UserAgent<T> {
    inner: T,
    user_agent: Option<HeaderValue>,
}

impl<T> UserAgent<T> {
    pub(crate) fn new(inner: T, user_agent: Option<HeaderValue>) -> Self {
        Self { inner, user_agent }
    }
}

impl<T, ReqBody> Service<Request<ReqBody>> for UserAgent<T>
where
    T: Service<Request<ReqBody>>,
{
    type Response = T::Response;
    type Error = T::Error;
    type Future = T::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<ReqBody>) -> Self::Future {
        let value = match self.user_agent.as_ref() {
            Some(custom_ua) => {
                let mut buf = Vec::new();
                buf.extend(custom_ua.as_bytes());
                buf.push(b' ');
                buf.extend(TONIC_USER_AGENT.as_bytes());
                HeaderValue::from_bytes(&buf).expect("user-agent should be valid")
            }
            None => HeaderValue::from_static(TONIC_USER_AGENT),
        };

        req.headers_mut().insert(USER_AGENT, value);

        self.inner.call(req)
    }
}
