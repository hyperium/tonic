use http::header::{self, HeaderName, HeaderValue};
use http::{HeaderMap, Method, Request, Response};
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
pub(crate) enum Direction {
    Request,
    Response,
}

#[derive(Copy, Clone, PartialEq, Debug)]
pub(crate) enum Encoding {
    Base64,
    None,
}

impl Encoding {
    pub(crate) fn from_content_type(headers: &HeaderMap) -> Encoding {
        Encoding::from_header(header::CONTENT_TYPE, headers)
    }

    pub(crate) fn from_accept(headers: &HeaderMap) -> Encoding {
        Encoding::from_header(header::ACCEPT, headers)
    }

    fn from_header(name: HeaderName, headers: &HeaderMap) -> Encoding {
        match header_value(name, headers) {
            Some(GRPC_WEB_TEXT_PROTO) | Some(GRPC_WEB_TEXT) => Encoding::Base64,
            _ => Encoding::None,
        }
    }

    pub(crate) fn to_content_type(&self) -> &'static str {
        match self {
            Encoding::Base64 => GRPC_WEB_TEXT_PROTO,
            Encoding::None => GRPC_WEB_PROTO,
        }
    }
}

pub(crate) fn is_grpc_web(headers: &HeaderMap) -> bool {
    matches!(
        content_type(headers),
        Some(GRPC_WEB) | Some(GRPC_WEB_PROTO) | Some(GRPC_WEB_TEXT) | Some(GRPC_WEB_TEXT_PROTO)
    )
}

pub(crate) fn is_grpc_web_preflight<B>(req: &Request<B>) -> bool {
    match header_value(header::ACCESS_CONTROL_REQUEST_HEADERS, req.headers()) {
        Some(value) => value.contains("x-grpc-web") && req.method() == Method::OPTIONS,
        None => false,
    }
}

pub(crate) fn content_type(headers: &HeaderMap) -> Option<&str> {
    header_value(header::CONTENT_TYPE, headers)
}

pub(crate) fn header_value(name: HeaderName, headers: &HeaderMap) -> Option<&str> {
    headers.get(name).and_then(|val| val.to_str().ok())
}

// Mutating request headers to conform to a gRPC request is not really
// necessary for us at this point. We could remove most of these except
// maybe for inserting `header::TE`, which tonic should check?
pub(crate) fn coerce_request(mut req: Request<Body>) -> Request<Body> {
    let encoding = Encoding::from_content_type(req.headers());

    req.headers_mut().remove(header::CONTENT_LENGTH);

    req.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(GRPC));

    req.headers_mut()
        .insert(header::TE, HeaderValue::from_static("trailers"));

    req.headers_mut().insert(
        header::ACCEPT_ENCODING,
        HeaderValue::from_static("identity,deflate,gzip"),
    );

    req.map(|b| GrpcWebCall::request(b, encoding))
        .map(Body::wrap_stream)
}

pub(crate) fn coerce_response(res: Response<BoxBody>, encoding: Encoding) -> Response<BoxBody> {
    let mut res = res
        .map(|b| GrpcWebCall::response(b, encoding))
        .map(BoxBody::new);

    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(encoding.to_content_type()),
    );

    res
}
