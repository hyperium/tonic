/// A gRPC Method info extension.
#[derive(Debug, Clone)]
pub struct GrpcMethod {
    service: &'static str,
    method: &'static str,
}

impl GrpcMethod {
    /// Create a new `GrpcMethod` extension.
    #[doc(hidden)]
    pub fn new(service: &'static str, method: &'static str) -> Self {
        Self { service, method }
    }

    /// gRPC service name
    pub fn service(&self) -> &str {
        self.service
    }
    /// gRPC method name
    pub fn method(&self) -> &str {
        self.method
    }
}
