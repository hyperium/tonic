//! Sets the URI scheme and authority on outgoing requests so the routing
//! layer can match against virtual hosts.
//!
//! When a tonic-generated client calls into the xDS channel, the outgoing
//! request URI is typically just the gRPC method path with no `:authority`.
//! The routing layer downstream matches `:authority` against the virtual
//! hosts in RDS — if it's empty, routing falls through to wildcard matches
//! only, breaking against control planes that return non-wildcard domains
//! (e.g. Istio's `*.svc.cluster.local`).
//!
//! Tonic's per-endpoint `Channel` has its own internal `AddOrigin`, but
//! that runs at the LB-selected endpoint layer (after routing) and is
//! keyed by the endpoint's IP:port, not the service-level authority — so
//! it can't substitute for this layer.
use std::task::{Context, Poll};

use http::{
    Request, Uri,
    uri::{Authority, Parts, Scheme},
};
use tower::{Layer, Service};

/// Layer that rewrites every request's URI scheme and authority.
#[derive(Clone, Debug)]
pub(crate) struct AddOriginLayer {
    scheme: Scheme,
    authority: Authority,
}

impl AddOriginLayer {
    pub(crate) fn new(scheme: Scheme, authority: Authority) -> Self {
        Self { scheme, authority }
    }

    /// Build an `AddOriginLayer` for the given authority string, defaulting
    /// to the HTTP scheme. Returns `None` (with a `tracing::warn`) if the
    /// string is not a valid URI authority — outgoing requests will then
    /// carry no `:authority` and non-wildcard virtual-host matching will
    /// fail at the routing layer.
    pub(crate) fn for_authority(authority: &str) -> Option<Self> {
        match Authority::try_from(authority) {
            Ok(parsed) => Some(Self::new(Scheme::HTTP, parsed)),
            Err(e) => {
                tracing::warn!(
                    target = authority,
                    error = %e,
                    "could not parse as URI authority; outgoing requests \
                     will carry no :authority"
                );
                None
            }
        }
    }
}

impl<S> Layer<S> for AddOriginLayer {
    type Service = AddOrigin<S>;

    fn layer(&self, inner: S) -> Self::Service {
        AddOrigin {
            inner,
            scheme: self.scheme.clone(),
            authority: self.authority.clone(),
        }
    }
}

/// Tower service that overwrites the URI scheme and authority of each
/// request before forwarding it. The path-and-query is preserved.
#[derive(Clone, Debug)]
pub(crate) struct AddOrigin<S> {
    inner: S,
    scheme: Scheme,
    authority: Authority,
}

impl<S, B> Service<Request<B>> for AddOrigin<S>
where
    S: Service<Request<B>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        // Mirrors tonic's per-endpoint AddOrigin layer
        // (channel/service/add_origin.rs). The Scheme/Authority clones are
        // refcount bumps on internal Bytes — no string copies.
        let (mut head, body) = req.into_parts();
        head.uri = {
            let mut parts: Parts = head.uri.into();
            parts.scheme = Some(self.scheme.clone());
            parts.authority = Some(self.authority.clone());
            Uri::from_parts(parts).expect("valid uri")
        };
        self.inner.call(Request::from_parts(head, body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;
    use tower::service_fn;

    fn capture_uri() -> (
        Arc<Mutex<Option<Uri>>>,
        impl Service<Request<()>, Response = (), Error = Infallible> + Clone,
    ) {
        let captured = Arc::new(Mutex::new(None));
        let captured_clone = captured.clone();
        let svc = service_fn(move |req: Request<()>| {
            *captured_clone.lock().unwrap() = Some(req.uri().clone());
            async move { Ok::<_, Infallible>(()) }
        });
        (captured, svc)
    }

    #[tokio::test]
    async fn rewrites_authority_and_scheme_preserving_path() {
        let (captured, inner) = capture_uri();
        let layer = AddOriginLayer::new(
            Scheme::HTTP,
            Authority::from_static("greeter.svc.cluster.local:50051"),
        );
        let svc = layer.layer(inner);

        let req = Request::builder()
            .uri("/some/path?query=1")
            .body(())
            .unwrap();
        svc.oneshot(req).await.unwrap();

        let uri = captured.lock().unwrap().clone().unwrap();
        assert_eq!(uri.scheme_str(), Some("http"));
        assert_eq!(
            uri.authority().map(|a| a.as_str()),
            Some("greeter.svc.cluster.local:50051")
        );
        assert_eq!(uri.path(), "/some/path");
        assert_eq!(uri.query(), Some("query=1"));
    }

    #[tokio::test]
    async fn replaces_existing_authority() {
        let (captured, inner) = capture_uri();
        let layer = AddOriginLayer::new(Scheme::HTTP, Authority::from_static("new.example:80"));
        let svc = layer.layer(inner);

        let req = Request::builder()
            .uri("http://old.example:443/rpc")
            .body(())
            .unwrap();
        svc.oneshot(req).await.unwrap();

        let uri = captured.lock().unwrap().clone().unwrap();
        assert_eq!(uri.authority().map(|a| a.as_str()), Some("new.example:80"));
        assert_eq!(uri.path(), "/rpc");
    }
}
