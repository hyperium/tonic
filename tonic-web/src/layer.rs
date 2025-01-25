use super::GrpcWebService;

use tower_layer::Layer;

/// Layer implementing the grpc-web protocol.
#[derive(Debug, Default, Clone)]
pub struct GrpcWebLayer {
    _priv: (),
}

impl GrpcWebLayer {
    /// Create a new grpc-web layer.
    pub fn new() -> GrpcWebLayer {
        Self::default()
    }
}

impl<S> Layer<S> for GrpcWebLayer {
    type Service = GrpcWebService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        GrpcWebService::new(inner)
    }
}
