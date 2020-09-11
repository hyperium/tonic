use http::header::{self, HeaderValue};
use http::{Method, Request, Response};
use hyper::Body;
use tonic::body::BoxBody;

mod grpc_web_call;
use grpc_web_call::GrpcWebCall;

// Grpc content types
const GRPC: &str = "application/grpc";
const GRPC_WEB: &str = "application/grpc-web";
const GRPC_WEB_PROTO: &str = "application/grpc-web+proto";
const GRPC_WEB_TEXT: &str = "application/grpc-web-text";
const GRPC_WEB_TEXT_PROTO: &str = "application/grpc-web-text+proto";

#[derive(Copy, Clone, PartialEq, Debug)]
pub enum Direction {
    Request,
    Response,
}

#[derive(Copy, Clone, PartialEq, Debug)]
pub enum Encoding {
    Base64,
    None,
}

impl Encoding {
    pub fn content_type(&self) -> &'static str {
        match self {
            Encoding::Base64 => GRPC_WEB_TEXT_PROTO,
            Encoding::None => GRPC_WEB_PROTO,
        }
    }
}

impl From<Option<&str>> for Encoding {
    fn from(value: Option<&str>) -> Self {
        match value {
            Some(GRPC_WEB_TEXT_PROTO) | Some(GRPC_WEB_TEXT) => Encoding::Base64,
            _ => Encoding::None,
        }
    }
}

pub(crate) fn is_grpc_web(req: &Request<Body>) -> bool {
    matches!(
        content_type(req),
        Some(GRPC_WEB) | Some(GRPC_WEB_PROTO) | Some(GRPC_WEB_TEXT) | Some(GRPC_WEB_TEXT_PROTO)
    )
}

pub(crate) fn is_grpc_web_preflight(req: &Request<Body>) -> bool {
    match req
        .headers()
        .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
        .and_then(|val| val.to_str().ok())
    {
        Some(value) => value.contains("x-grpc-web") && req.method() == Method::OPTIONS,
        None => false,
    }
}

pub(crate) fn content_type(req: &Request<hyper::Body>) -> Option<&str> {
    req.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|ct| ct.to_str().ok())
}

pub(crate) fn accept(req: &Request<hyper::Body>) -> Option<&str> {
    req.headers()
        .get(header::ACCEPT)
        .and_then(|val| val.to_str().ok())
}

pub(crate) fn coerce_request(mut req: Request<Body>) -> Request<Body> {
    let request_encoding = Encoding::from(content_type(&req));

    req.headers_mut().remove(header::CONTENT_LENGTH);

    req.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(GRPC));

    req.headers_mut()
        .insert(header::TE, HeaderValue::from_static("trailers"));

    req.headers_mut().insert(
        header::ACCEPT_ENCODING,
        HeaderValue::from_static("identity,deflate,gzip"),
    );

    req.map(|b| GrpcWebCall::new(b, Direction::Request, request_encoding))
        .map(Body::wrap_stream)
}

pub(crate) fn coerce_response(res: Response<BoxBody>, encoding: Encoding) -> Response<BoxBody> {
    let mut res = res
        .map(|b| GrpcWebCall::new(b, Direction::Response, encoding))
        .map(BoxBody::new);

    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(encoding.content_type()),
    );

    res
}
