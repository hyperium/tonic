use super::{BoxBody, BoxError, Config, GrpcWeb};

use tower_layer::Layer;
use tower_service::Service;

/// Layer implementing the grpc-web protocol.
#[derive(Debug, Clone)]
pub struct GrpcWebLayer {
    _priv: (),
}

impl GrpcWebLayer {
    /// Create a new grpc-web layer.
    pub fn new() -> GrpcWebLayer {
        Self { _priv: () }
    }
}

impl<S> Layer<S> for GrpcWebLayer
where
    S: Service<http::Request<hyper::Body>, Response = http::Response<BoxBody>>,
    S: Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError> + Send,
{
    type Service = GrpcWeb<S>;

    fn layer(&self, inner: S) -> Self::Service {
        Config::default().enable(inner)
    }
}
