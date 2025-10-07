use std::error::Error;

use super::{BoxError, GrpcWebService};
use tonic::body::Body;

use tower_layer::Layer;
use tower_service::Service;

/// Layer implementing the grpc-web protocol.
#[derive(Debug)]
pub struct GrpcWebLayer<ResBody = Body> {
    _markers: std::marker::PhantomData<fn() -> ResBody>,
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
    S: Service<http::Request<Body>, Response = http::Response<ResBody>> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError> + Send,
    ResBody: http_body::Body<Data = bytes::Bytes> + Send + 'static,
    ResBody::Error: Error + Send + Sync + 'static,
{
    type Service = GrpcWebService<S, ResBody>;

    fn layer(&self, inner: S) -> Self::Service {
        GrpcWebService::new(inner)
    }
}
