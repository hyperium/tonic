use crate::cors_headers::*;
use http::{header, header::HeaderName, HeaderMap, HeaderValue, Method, Response, StatusCode};
use std::{
    collections::{BTreeSet, HashSet},
    convert::TryFrom,
    iter::FromIterator,
    time::Duration,
};
use tonic::body::BoxBody;

const DEFAULT_EXPOSED_HEADERS: [&str; 2] = ["grpc-status", "grpc-message"];
const DEFAULT_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const DEFAULT_ALLOWED_METHODS: &[Method; 2] = &[Method::POST, Method::OPTIONS];

/// Specifies which origins are allowed to access this resource
#[derive(Debug, Clone)]
pub enum AllowedOrigins {
    // Any origin is allowed
    Any,

    // Allow a specific set of origins
    Origins(BTreeSet<HeaderValue>),
}

impl AllowedOrigins {
    fn origin_allowed(&self, origin: &HeaderValue) -> bool {
        match self {
            AllowedOrigins::Any => true,
            AllowedOrigins::Origins(origins) => origins.contains(origin),
        }
    }
}

impl<A> FromIterator<A> for AllowedOrigins
where
    A: Into<HeaderValue>,
{
    fn from_iter<I>(iter: I) -> Self
    where
        I: IntoIterator<Item = A>,
    {
        let origins = iter.into_iter().map(Into::into).collect();

        AllowedOrigins::Origins(origins)
    }
}

#[derive(Debug, PartialEq)]
pub enum Error {
    OriginNotAllowed,
    MethodNotAllowed,
    MissingRequestMethod,
    InvalidMethod,
    InvalidHeader,
}

impl Error {
    pub fn into_response(self) -> http::Response<BoxBody> {
        use Error::*;

        match self {
            MethodNotAllowed | OriginNotAllowed | InvalidMethod => {
                http_response(StatusCode::FORBIDDEN)
            }
            MissingRequestMethod => http_response(StatusCode::FORBIDDEN),
            InvalidHeader => panic!("how did we get here?"),
        }
    }
}

fn http_response(status: StatusCode) -> http::Response<BoxBody> {
    Response::builder()
        .status(status)
        .body(BoxBody::empty())
        .unwrap()
}

#[derive(Debug, Clone)]
pub struct Builder {
    allowed_origins: AllowedOrigins,
    exposed_headers: HashSet<HeaderName>,
    max_age: Option<Duration>,
    allow_credentials: bool,
}

#[derive(Debug, Clone)]
pub struct Cors {
    enabled: bool,
    builder: Builder,
    exposed_headers: HeaderValue,
    allowed_methods: HeaderValue,
}

impl Cors {
    pub fn builder() -> Builder {
        Builder::new()
    }

    pub fn disabled() -> Cors {
        Cors {
            enabled: false,
            builder: Default::default(),
            exposed_headers: HeaderValue::from_static(""),
            allowed_methods: HeaderValue::from_static(""),
        }
    }

    fn common_headers(&self, origin: HeaderValue) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(ALLOW_ORIGIN, origin);
        headers.insert(EXPOSE_HEADERS, self.exposed_headers.clone());

        // TODO: maybe cache this header?
        if self.builder.allow_credentials {
            headers.insert(ALLOW_CREDENTIALS, HeaderValue::from_static("true"));
        }

        headers
    }

    #[cfg(test)]
    pub fn __check_preflight(&self, headers: &HeaderMap) -> Result<HeaderMap, Error> {
        self.check_preflight(
            headers,
            headers.get(ORIGIN).unwrap(),
            headers.get(REQUEST_HEADERS).unwrap(),
        )
    }

    fn is_method_allowed(&self, header: Option<&HeaderValue>) -> bool {
        match header {
            Some(value) => Method::from_bytes(value.as_bytes())
                .map(|method| DEFAULT_ALLOWED_METHODS.contains(&method))
                .unwrap_or(false),
            None => false,
        }
    }

    pub fn check_preflight(
        &self,
        req_headers: &HeaderMap,
        origin: &HeaderValue,
        request_headers_header: &HeaderValue,
    ) -> Result<HeaderMap, Error> {
        if !self.is_origin_allowed(origin) {
            return Err(Error::OriginNotAllowed);
        }

        if !self.is_method_allowed(req_headers.get(REQUEST_METHOD)) {
            return Err(Error::MethodNotAllowed);
        }

        let mut headers = self.common_headers(origin.clone());
        headers.insert(ALLOW_METHODS, self.allowed_methods.clone());
        headers.insert(ALLOW_HEADERS, request_headers_header.clone());

        if let Some(max_age) = self.builder.max_age {
            headers.insert(MAX_AGE, HeaderValue::from(max_age.as_secs()));
        }

        Ok(headers)
    }

    pub fn check_simple(&self, headers: &HeaderMap) -> Result<HeaderMap, Error> {
        if !self.enabled {
            return Ok(HeaderMap::new());
        }

        match headers.get(header::ORIGIN) {
            Some(origin) if self.is_origin_allowed(origin) => {
                Ok(self.common_headers(origin.clone()))
            }
            Some(_) => Err(Error::OriginNotAllowed),
            None => Ok(HeaderMap::new()),
        }
    }

    fn is_origin_allowed(&self, origin: &HeaderValue) -> bool {
        self.builder.allowed_origins.origin_allowed(origin)
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

    pub fn allow_origin(self, origin: &'static str) -> Builder {
        let mut set = BTreeSet::new();
        set.insert(HeaderValue::from_static(origin));
        let allowed_origins = AllowedOrigins::Origins(set);

        Self {
            allowed_origins,
            ..self
        }
    }

    pub fn allow_origins(self, allowed_origins: AllowedOrigins) -> Builder {
        Self {
            allowed_origins,
            ..self
        }
    }

    pub fn expose_header<T>(mut self, header: T) -> Builder
    where
        HeaderName: TryFrom<T>,
    {
        let header = match TryFrom::try_from(header) {
            Ok(m) => m,
            Err(_) => panic!("illegal Header"),
        };
        self.exposed_headers.insert(header);
        self
    }

    pub fn expose_headers<I>(mut self, headers: I) -> Builder
    where
        I: IntoIterator,
        HeaderName: TryFrom<I::Item>,
    {
        let iter = headers
            .into_iter()
            .map(|header| match TryFrom::try_from(header) {
                Ok(header) => header,
                Err(_) => panic!("invalid header"),
            });

        self.exposed_headers.extend(iter);
        self
    }

    pub fn max_age<T: Into<Option<Duration>>>(self, max_age: T) -> Builder {
        Self {
            max_age: max_age.into(),
            ..self
        }
    }

    pub fn allow_credentials(self, allow_credentials: bool) -> Builder {
        Self {
            allow_credentials,
            ..self
        }
    }

    pub fn build(self) -> Cors {
        let exposed_headers = join_header_value(&self.exposed_headers).unwrap();
        let allowed_methods = HeaderValue::from_static("POST,OPTIONS");

        Cors {
            enabled: true,
            builder: self,
            exposed_headers,
            allowed_methods,
        }
    }
}

// TODO: this is the other way around: default calls new()
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
            allow_credentials: true,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_value_eq(actual: &HeaderValue, expected: &str) {
        fn sorted(value: &str) -> Vec<&str> {
            let mut vec = value.split(",").collect::<Vec<_>>();
            vec.sort();
            vec
        }

        assert_eq!(sorted(actual.to_str().unwrap()), sorted(expected))
    }

    fn value(s: &str) -> HeaderValue {
        s.parse().unwrap()
    }

    mod preflight {
        use super::*;

        fn preflight_headers() -> HeaderMap {
            let mut headers = HeaderMap::new();
            headers.insert(ORIGIN, value("http://example.com"));
            headers.insert(REQUEST_METHOD, value("POST"));
            headers.insert(REQUEST_HEADERS, value("x-grpc-web"));
            headers
        }

        #[test]
        fn default_config() {
            let cors = Cors::default();
            let headers = cors.__check_preflight(&preflight_headers()).unwrap();

            assert_eq!(headers[ALLOW_ORIGIN], "http://example.com");
            assert_eq!(headers[ALLOW_METHODS], "POST,OPTIONS");
            assert_eq!(headers[ALLOW_HEADERS], "x-grpc-web");
            assert_eq!(headers[ALLOW_CREDENTIALS], "true");
            assert_eq!(headers[MAX_AGE], "86400");
            assert_value_eq(&headers[EXPOSE_HEADERS], "grpc-status,grpc-message");
        }

        #[test]
        fn any_origin() {
            let cors = Cors::builder().allow_origins(AllowedOrigins::Any).build();

            assert!(cors.__check_preflight(&preflight_headers()).is_ok());
        }

        #[test]
        fn origin_list() {
            let cors = Cors::builder()
                .allow_origins(AllowedOrigins::from_iter(vec![
                    HeaderValue::from_static("http://a.com"),
                    HeaderValue::from_static("http://b.com"),
                ]))
                .build();

            let mut req_headers = preflight_headers();
            req_headers.insert(ORIGIN, value("http://b.com"));

            assert!(cors.__check_preflight(&req_headers).is_ok());
        }

        #[test]
        fn origin_not_allowed() {
            let cors = Cors::builder().allow_origin("http://a.com").build();
            let err = cors.__check_preflight(&preflight_headers()).unwrap_err();

            assert_eq!(err, Error::OriginNotAllowed)
        }

        #[test]
        fn disallow_credentials() {
            let cors = Cors::builder().allow_credentials(false).build();
            let headers = cors.__check_preflight(&preflight_headers()).unwrap();

            assert!(!headers.contains_key(ALLOW_CREDENTIALS));
        }

        #[test]
        fn expose_headers_are_merged() {
            let cors = Cors::builder().expose_headers(vec!["x-request-id"]).build();
            let headers = cors.__check_preflight(&preflight_headers()).unwrap();

            assert_value_eq(
                &headers[EXPOSE_HEADERS],
                "x-request-id,grpc-message,grpc-status",
            );
        }

        #[test]
        fn allow_headers_echo_request_headers() {
            let cors = Cors::default();
            let mut request_headers = preflight_headers();
            request_headers.insert(REQUEST_HEADERS, value("x-grpc-web,foo,x-request-id"));

            let headers = cors.__check_preflight(&request_headers).unwrap();

            assert_value_eq(&headers[ALLOW_HEADERS], "x-grpc-web,foo,x-request-id")
        }

        #[test]
        #[ignore]
        fn missing_request_method() {
            let cors = Cors::default();
            let mut request_headers = preflight_headers();
            request_headers.remove(REQUEST_METHOD);

            let err = cors.__check_preflight(&request_headers).unwrap_err();

            assert_eq!(err, Error::MissingRequestMethod);
        }

        #[test]
        fn only_options_and_post_allowed() {
            let cors = Cors::default();

            for method in &[
                Method::GET,
                Method::DELETE,
                Method::TRACE,
                Method::PATCH,
                Method::PUT,
                Method::HEAD,
            ] {
                let mut request_headers = preflight_headers();
                request_headers.insert(REQUEST_METHOD, value(method.as_str()));

                assert_eq!(
                    cors.__check_preflight(&request_headers).unwrap_err(),
                    Error::MethodNotAllowed,
                )
            }
        }

        #[test]
        fn custom_max_age() {
            use std::time::Duration;

            let cors = Cors::builder().max_age(Duration::from_secs(99)).build();
            let headers = cors.__check_preflight(&preflight_headers()).unwrap();

            assert_eq!(headers[MAX_AGE], "99");
        }

        #[test]
        fn no_max_age() {
            let cors = Cors::builder().max_age(None).build();
            let headers = cors.__check_preflight(&preflight_headers()).unwrap();

            assert!(!headers.contains_key(MAX_AGE));
        }
    }

    mod simple {
        use super::*;

        fn request_headers() -> HeaderMap {
            let mut headers = HeaderMap::new();
            headers.insert(ORIGIN, value("http://example.com"));
            headers
        }

        #[test]
        fn default_config() {
            let cors = Cors::default();
            let headers = cors.check_simple(&request_headers()).unwrap();

            assert_eq!(headers[ALLOW_ORIGIN], "http://example.com");
            assert_eq!(headers[ALLOW_CREDENTIALS], "true");
            assert_value_eq(&headers[EXPOSE_HEADERS], "grpc-message,grpc-status");

            assert!(!headers.contains_key(ALLOW_HEADERS));
            assert!(!headers.contains_key(ALLOW_METHODS));
            assert!(!headers.contains_key(MAX_AGE));
        }

        #[test]
        fn any_origin() {
            let cors = Cors::builder().allow_origins(AllowedOrigins::Any).build();

            assert!(cors.check_simple(&request_headers()).is_ok());
        }

        #[test]
        fn origin_list() {
            let cors = Cors::builder()
                .allow_origins(AllowedOrigins::from_iter(vec![
                    HeaderValue::from_static("http://a.com"),
                    HeaderValue::from_static("http://b.com"),
                ]))
                .build();

            let mut req_headers = request_headers();
            req_headers.insert(ORIGIN, value("http://b.com"));

            assert!(cors.check_simple(&req_headers).is_ok());
        }

        #[test]
        fn origin_not_allowed() {
            let cors = Cors::builder().allow_origin("http://a.com").build();
            let err = cors.check_simple(&request_headers()).unwrap_err();

            assert_eq!(err, Error::OriginNotAllowed)
        }

        #[test]
        fn disallow_credentials() {
            let cors = Cors::builder().allow_credentials(false).build();
            let headers = cors.check_simple(&request_headers()).unwrap();

            assert!(!headers.contains_key(ALLOW_CREDENTIALS));
        }

        #[test]
        fn expose_headers_are_merged() {
            let cors = Cors::builder()
                .expose_header("x-hello")
                .expose_headers(vec!["custom-1"])
                .build();

            let headers = cors.check_simple(&request_headers()).unwrap();

            assert_value_eq(
                &headers[EXPOSE_HEADERS],
                "grpc-message,grpc-status,x-hello,custom-1",
            );
        }
    }
}
