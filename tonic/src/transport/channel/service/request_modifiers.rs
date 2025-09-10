use crate::body::Body;
use http::{header::USER_AGENT, HeaderValue, Request, Uri};
use std::task::{Context, Poll};
use tower_service::Service;

/// A generic request modifier.
///
/// `Modifier<M, T>` wraps an inner service `T` and applies the
/// modifier `M` to each outgoing `Request`.
///
/// This type centralizes the boilerplate for implementing
/// request middleware. A modifier is closure which receives
/// the request and mutates it before forwarding to the
/// inner service.
#[derive(Debug)]
pub(crate) struct Modifier<M, T> {
    modifier_fn: M,
    next: T,
}

impl<M, T> Modifier<M, T> {
    pub(crate) fn new(next: T, modifier_fn: M) -> Self {
        Self { next, modifier_fn }
    }
}

impl<M, Body, T> Service<Request<Body>> for Modifier<M, T>
where
    T: Service<Request<Body>>,
    M: FnOnce(Request<Body>) -> Request<Body> + Clone,
    Body: Send + 'static,
{
    type Response = T::Response;
    type Error = T::Error;
    type Future = T::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.next.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let modifier_fn = self.modifier_fn.clone();
        self.next.call(modifier_fn(req))
    }
}

// We're borrowing to avoid cloning the Uri more than once in layer which expects Fn
// and not FnOnce
#[derive(Debug, Clone)]
pub(crate) struct AddOrigin<'a> {
    origin: &'a Uri,
}

impl<'a> AddOrigin<'a> {
    pub(crate) fn new(origin: &'a Uri) -> Result<Self, crate::BoxError> {
        // We catch error right at initiation... This single line
        // eliminates countless heap allocations at `runtime`
        if origin.scheme().is_none() || origin.authority().is_none() {
            return Err(crate::transport::Error::new_invalid_uri().into());
        }

        Ok(Self { origin })
    }

    pub(crate) fn into_fn(self) -> impl FnOnce(Request<Body>) -> Request<Body> + Clone {
        let http::uri::Parts {
            scheme, authority, ..
        } = self.origin.clone().into_parts();

        // Both have been checked
        let scheme = scheme.unwrap();
        let authority = authority.unwrap();

        move |req| {
            // Split the request into the head and the body.
            let (mut head, body) = req.into_parts();

            // Update the request URI
            head.uri = {
                // Split the request URI into parts.
                let mut uri: http::uri::Parts = head.uri.into();
                // Update the URI parts, setting the scheme and authority
                uri.scheme = Some(scheme);
                uri.authority = Some(authority);

                http::Uri::from_parts(uri).expect("valid uri")
            };

            Request::from_parts(head, body)
        }
    }
}

const TONIC_USER_AGENT: &str = concat!("tonic/", env!("CARGO_PKG_VERSION"));

#[derive(Debug)]
pub(crate) struct UserAgent {
    user_agent: HeaderValue,
}

impl UserAgent {
    pub(crate) fn new(user_agent: Option<HeaderValue>) -> Self {
        let user_agent = user_agent
            .map(|value| {
                let mut buf = Vec::new();
                buf.extend(value.as_bytes());
                buf.push(b' ');
                buf.extend(TONIC_USER_AGENT.as_bytes());
                HeaderValue::from_bytes(&buf).expect("user-agent should be valid")
            })
            .unwrap_or_else(|| HeaderValue::from_static(TONIC_USER_AGENT));

        Self { user_agent }
    }

    pub(crate) fn into_fn(self) -> impl FnOnce(Request<Body>) -> Request<Body> + Clone {
        move |mut req| {
            use http::header::Entry;

            // The former code uses try_insert so we'll respect that
            if let Ok(entry) = req.headers_mut().try_entry(USER_AGENT) {
                // This is to avoid anticipative cloning which happened
                // in the former code
                match entry {
                    Entry::Vacant(vacant_entry) => {
                        vacant_entry.insert(self.user_agent);
                    }
                    Entry::Occupied(occupied_entry) => {
                        // The User-Agent header has already been set on the request. Let's
                        // append our user agent to the end.
                        let occupied_entry = occupied_entry.into_mut();

                        let mut buf =
                            Vec::with_capacity(occupied_entry.len() + 1 + self.user_agent.len());
                        buf.extend(occupied_entry.as_bytes());
                        buf.push(b' ');
                        buf.extend(self.user_agent.as_bytes());

                        // with try_into http uses from_shared internally to probably minimize
                        // allocations
                        *occupied_entry = buf.try_into().expect("user-agent should be valid")
                    }
                }
            }

            req
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_default_if_no_custom_user_agent() {
        assert_eq!(
            UserAgent::new(None).user_agent,
            HeaderValue::from_static(TONIC_USER_AGENT)
        )
    }

    #[test]
    fn prepends_custom_user_agent_to_default() {
        assert_eq!(
            UserAgent::new(Some(HeaderValue::from_static("Greeter 1.1"))).user_agent,
            HeaderValue::from_str(&format!("Greeter 1.1 {TONIC_USER_AGENT}")).unwrap()
        )
    }

    async fn assert_user_agent_modified(
        genesis_user_agent: Option<impl TryInto<HeaderValue>>,
        expected_user_agent: impl TryInto<HeaderValue>,
        request: Option<Request<Body>>,
    ) {
        let ua = UserAgent::new(genesis_user_agent.map(|v| {
            v.try_into()
                .unwrap_or_else(|_| panic!("invalid header value"))
        }))
        .into_fn();

        let modified_request = ua(request.unwrap_or_default());
        let user_agent = modified_request.headers().get(USER_AGENT).unwrap();
        assert_eq!(
            user_agent,
            expected_user_agent
                .try_into()
                .unwrap_or_else(|_| panic!("invalid header value"))
        );
    }

    #[tokio::test]
    async fn sets_default_user_agent_if_none_present() {
        let genesis_user_agent = Option::<&str>::None;
        let expected_user_agent = TONIC_USER_AGENT.to_string();
        let request = None;

        assert_user_agent_modified(genesis_user_agent, expected_user_agent, request).await
    }

    #[tokio::test]
    async fn sets_custom_user_agent_if_none_present() {
        let genesis_user_agent = Some("Greeter 1.1");
        let expected_user_agent = format!("Greeter 1.1 {TONIC_USER_AGENT}");
        let request = None;

        assert_user_agent_modified(genesis_user_agent, expected_user_agent, request).await
    }

    #[tokio::test]
    async fn appends_default_user_agent_to_request_fn_user_agent() {
        let genesis_user_agent = Option::<&str>::None;
        let expected_user_agent = format!("request-ua/x.y {TONIC_USER_AGENT}");
        let mut request = Request::default();
        request
            .headers_mut()
            .insert(USER_AGENT, HeaderValue::from_static("request-ua/x.y"));

        assert_user_agent_modified(genesis_user_agent, expected_user_agent, Some(request)).await
    }

    #[tokio::test]
    async fn appends_custom_user_agent_to_request_fn_user_agent() {
        let genesis_user_agent = Some("Greeter 1.1");
        let expected_user_agent = format!("request-ua/x.y Greeter 1.1 {TONIC_USER_AGENT}");
        let mut request = Request::default();
        request
            .headers_mut()
            .insert(USER_AGENT, HeaderValue::from_static("request-ua/x.y"));

        assert_user_agent_modified(genesis_user_agent, expected_user_agent, Some(request)).await
    }
}
