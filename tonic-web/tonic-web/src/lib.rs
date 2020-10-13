mod call;

pub use cors::Cors;
mod cors;

mod service;
pub use service::GrpcWeb;

mod content_types {
    pub(crate) const GRPC: &str = "application/grpc";
    pub(crate) const GRPC_WEB: &str = "application/grpc-web";
    pub(crate) const GRPC_WEB_PROTO: &str = "application/grpc-web+proto";
    pub(crate) const GRPC_WEB_TEXT: &str = "application/grpc-web-text";
    pub(crate) const GRPC_WEB_TEXT_PROTO: &str = "application/grpc-web-text+proto";
}

mod cors_headers {
    pub(crate) use http::header::ACCESS_CONTROL_ALLOW_CREDENTIALS as ALLOW_CREDENTIALS;
    pub(crate) use http::header::ACCESS_CONTROL_ALLOW_HEADERS as ALLOW_HEADERS;
    pub(crate) use http::header::ACCESS_CONTROL_ALLOW_METHODS as ALLOW_METHODS;
    pub(crate) use http::header::ACCESS_CONTROL_ALLOW_ORIGIN as ALLOW_ORIGIN;
    pub(crate) use http::header::ACCESS_CONTROL_EXPOSE_HEADERS as EXPOSE_HEADERS;
    pub(crate) use http::header::ACCESS_CONTROL_MAX_AGE as MAX_AGE;
    pub(crate) use http::header::ACCESS_CONTROL_REQUEST_HEADERS as REQUEST_HEADERS;
    pub(crate) use http::header::ACCESS_CONTROL_REQUEST_METHOD as REQUEST_METHOD;
    pub(crate) use http::header::ORIGIN;
}
