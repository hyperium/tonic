#![allow(unused)]
use std::collections::{BTreeSet, HashSet};
use std::time::Duration;

use http::{header, header::HeaderName, method, HeaderMap, HeaderValue, Method, Request};

const DEFAULT_EXPOSED_HEADERS: [&str; 2] = ["grpc-status", "grpc-message"];
const TWENTY_FOUR_HOURS: u64 = 24 * 60 * 60;
const DEFAULT_MAX_AGE: Duration = Duration::from_secs(TWENTY_FOUR_HOURS);
const DEFAULT_ALLOWED_METHODS: &[Method; 2] = &[Method::POST, Method::OPTIONS];

/// Specifies which origins are allowed to access this resource
#[derive(Debug, Clone)]
pub enum AllowedOrigins {
    // Any origin is allowed
    Any,

    // Allow a specific set of origins
    Origins(BTreeSet<HeaderValue>),
}

//
impl AllowedOrigins {
    fn origin_allowed(&self, origin: &HeaderValue) -> bool {
        match self {
            AllowedOrigins::Any => true,
            AllowedOrigins::Origins(origins) => origins.contains(origin),
        }
    }
}

#[doc(hidden)]
#[derive(Debug)]
pub enum CorsResource {
    Preflight(HeaderMap),
    Simple(HeaderMap),
}

#[doc(hidden)]
#[derive(Debug)]
pub enum InvalidRequest {
    OriginNotAllowed,
    MethodNotAllowed,
    InvalidMethod(method::InvalidMethod),
    HeaderNotAllowed,
    InvalidHeader(header::InvalidHeaderName),
}

#[derive(Debug, Clone)]
pub struct Builder {
    allowed_origins: AllowedOrigins,
    exposed_headers: HashSet<HeaderName>,
    max_age: Option<Duration>,
}

#[derive(Debug, Clone)]
pub struct Cors {
    builder: Builder,
    exposed_headers: HeaderValue,
    allowed_methods: HeaderValue,
}

impl Cors {
    pub fn builder() -> Builder {
        Builder::new()
    }

    pub fn process_request<B>(&self, req: &Request<B>) -> Result<CorsResource, InvalidRequest> {
        use self::InvalidRequest::*;

        let origin = req.headers().get(header::ORIGIN);

        let method = req.headers().get(header::ACCESS_CONTROL_REQUEST_METHOD);

        let requested_headers = req.headers().get(header::ACCESS_CONTROL_REQUEST_HEADERS);

        match (origin, req.method(), method) {
            (None, _, _) => Ok(CorsResource::Simple(self.basic_headers())),
            (Some(origin), &Method::OPTIONS, Some(_method)) => {
                let mut headers = HeaderMap::new();
                headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());

                headers.insert(
                    header::ACCESS_CONTROL_ALLOW_METHODS,
                    HeaderValue::from_static("POST,OPTIONS"),
                );

                headers.insert(
                    header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
                    HeaderValue::from_static("true"),
                );

                if let Some(req_headers) = requested_headers {
                    headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, req_headers.clone());
                }

                headers.insert(
                    header::ACCESS_CONTROL_MAX_AGE,
                    HeaderValue::from_static("86400"),
                );

                Ok(CorsResource::Preflight(headers))
            }
            (Some(origin), _, _) => {
                let mut headers = HeaderMap::new();
                headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());

                headers.insert(
                    header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
                    HeaderValue::from_static("true"),
                );

                headers.insert(
                    header::ACCESS_CONTROL_EXPOSE_HEADERS,
                    self.exposed_headers.clone(),
                );

                Ok(CorsResource::Simple(headers))
            }
        }
    }

    fn is_origin_allowed(&self, origin: &HeaderValue) -> bool {
        match &self.builder.allowed_origins {
            AllowedOrigins::Any => true,
            AllowedOrigins::Origins(origins) => origins.contains(origin),
        }
    }

    fn is_method_allowed(&self, method: &Method) -> bool {
        const ALLOWED_METHODS: [Method; 2] = [Method::POST, Method::OPTIONS];

        ALLOWED_METHODS.contains(method)
    }

    fn basic_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        // headers.insert(header::VARY, self.vary.clone());
        headers
    }
}

impl Default for Cors {
    fn default() -> Self {
        Builder::default().build()
    }
}

impl Builder {
    pub fn new() -> Builder {
        Builder::default()
    }

    pub fn allow_origins(self) -> Builder {
        self
    }

    pub fn expose_headers(self) -> Builder {
        self
    }

    pub fn max_age(self) -> Builder {
        self
    }

    pub fn build(self) -> Cors {
        let exposed_headers = join_header_value(&self.exposed_headers).unwrap();

        let allowed_methods = HeaderValue::from_static("POST,OPTIONS");

        Cors {
            builder: self,
            exposed_headers,
            allowed_methods,
        }
    }
}

impl Default for Builder {
    fn default() -> Self {
        Builder {
            allowed_origins: AllowedOrigins::Any,
            exposed_headers: DEFAULT_EXPOSED_HEADERS
                .iter()
                .cloned()
                .map(HeaderName::from_static)
                .collect(),
            max_age: Some(DEFAULT_MAX_AGE),
        }
    }
}

fn join_header_value<I>(values: I) -> Result<HeaderValue, header::InvalidHeaderValue>
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    let mut values = values.into_iter();
    let mut value = Vec::new();

    if let Some(v) = values.next() {
        value.extend(v.as_ref().as_bytes());
    }
    for v in values {
        value.push(b',');
        value.extend(v.as_ref().as_bytes());
    }
    HeaderValue::from_bytes(&value)
}
