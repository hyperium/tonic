use super::GrpcWebService;

use tower_layer::Layer;

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

impl<S> Layer<S> for GrpcWebLayer {
    type Service = GrpcWebService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        GrpcWebService::new(inner)
    }
}
