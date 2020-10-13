use crate::{
    content_types::*,
    cors_headers::{ORIGIN, REQUEST_HEADERS},
};
use http::{
    header::{self, HeaderValue},
    HeaderMap, Method, Request, Response, Version,
};
use hyper::Body;
use tonic::body::BoxBody;

use grpc_web_call::GrpcWebCall;
mod grpc_web_call;

fn is_grpc_web(headers: &HeaderMap) -> bool {
    matches!(
        content_type(headers),
        Some(GRPC_WEB) | Some(GRPC_WEB_PROTO) | Some(GRPC_WEB_TEXT) | Some(GRPC_WEB_TEXT_PROTO)
    )
}

// TODO: this should be moved out of here, to service or util
pub(crate) fn classify_request<'a>(
    headers: &'a HeaderMap,
    method: &'a Method,
    version: Version,
) -> RequestKind<'a> {
    if is_grpc_web(headers) {
        return RequestKind::GrpcWeb(method);
    }

    let req_headers = headers.get(REQUEST_HEADERS);
    let origin = headers.get(ORIGIN);

    match (method, origin, req_headers) {
        (&Method::OPTIONS, Some(origin), Some(value)) => match value.to_str() {
            Ok(h) if h.contains("x-grpc-web") => {
                println!("THINKS ITS VALID PRE-FLIGHT");

                return RequestKind::GrpcWebPreflight {
                    origin,
                    request_headers: value,
                };
            }
            _ => {}
        },
        _ => {}
    }

    RequestKind::Other(version)
}

#[derive(Debug, PartialEq)]
pub(crate) enum RequestKind<'a> {
    GrpcWeb(&'a Method),
    GrpcWebPreflight {
        origin: &'a HeaderValue,
        request_headers: &'a HeaderValue,
    },
    Other(http::Version),
}

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
        Self::from_header(headers.get(header::CONTENT_TYPE))
    }

    pub(crate) fn from_accept(headers: &HeaderMap) -> Encoding {
        Self::from_header(headers.get(header::ACCEPT))
    }

    fn from_header(value: Option<&HeaderValue>) -> Encoding {
        match value.and_then(|val| val.to_str().ok()) {
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

pub(crate) fn content_type(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|val| val.to_str().ok())
}

// Mutating request headers to conform to a gRPC request is not really
// necessary for us at this point. We could remove most of these except
// maybe for inserting `header::TE`, which tonic should check?
pub(crate) fn coerce_request(mut req: Request<Body>, encoding: Encoding) -> Request<Body> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_constructors() {
        let cases = &[
            (GRPC_WEB, Encoding::None),
            (GRPC_WEB_PROTO, Encoding::None),
            (GRPC_WEB_TEXT, Encoding::Base64),
            (GRPC_WEB_TEXT_PROTO, Encoding::Base64),
            ("foo", Encoding::None),
        ];

        let mut headers = HeaderMap::new();

        for case in cases {
            headers.insert(header::CONTENT_TYPE, case.0.parse().unwrap());
            headers.insert(header::ACCEPT, case.0.parse().unwrap());

            assert_eq!(Encoding::from_content_type(&headers), case.1, "{}", case.0);
            assert_eq!(Encoding::from_accept(&headers), case.1, "{}", case.0);
        }
    }
}
