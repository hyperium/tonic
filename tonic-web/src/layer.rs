use super::{BoxBody, BoxError, GrpcWebService};

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

impl Default for GrpcWebLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Layer<S> for GrpcWebLayer
where
    S: Service<http::Request<hyper::Body>, Response = http::Response<BoxBody>>,
    S: Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError> + Send,
{
    type Service = GrpcWebService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        GrpcWebService::new(inner)
    }
}
