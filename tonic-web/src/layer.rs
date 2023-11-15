use std::error::Error;

use super::{BoxBody, BoxError, GrpcWebService};

use tower_layer::Layer;
use tower_service::Service;

/// Layer implementing the grpc-web protocol.
#[derive(Debug)]
pub struct GrpcWebLayer<ResBody = BoxBody> {
    _markers: std::marker::PhantomData<ResBody>,
}

impl<ResBody> Clone for GrpcWebLayer<ResBody> {
    fn clone(&self) -> Self {
        Self {
            _markers: std::marker::PhantomData,
        }
    }
}

impl<ResBody> GrpcWebLayer<ResBody> {
    /// Create a new grpc-web layer.
    pub fn new() -> Self {
        Self {
            _markers: std::marker::PhantomData,
        }
    }
}

impl<ResBody> Default for GrpcWebLayer<ResBody> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S, ResBody> Layer<S> for GrpcWebLayer<ResBody>
where
    S: Service<http::Request<BoxBody>, Response = http::Response<ResBody>> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError> + Send,
    ResBody: http_body::Body + Send + 'static,
    ResBody::Error: Error + Send + 'static,
{
    type Service = GrpcWebService<S, ResBody>;

    fn layer(&self, inner: S) -> Self::Service {
        GrpcWebService::new(inner)
    }
}
