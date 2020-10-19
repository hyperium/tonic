use std::sync::Arc;

use http::{header, HeaderMap, HeaderValue, Method};
use tracing::debug;

use self::headers::*;
use crate::config::Config;

pub(crate) mod headers {
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

const DEFAULT_ALLOWED_METHODS: &[Method; 2] = &[Method::POST, Method::OPTIONS];

#[derive(Debug, Clone)]
pub(crate) struct Cors {
    cache: Arc<Cache>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Error {
    OriginNotAllowed,
    MethodNotAllowed,
}

#[derive(Clone, Debug)]
struct Cache {
    config: Config,
    expose_headers: HeaderValue,
    allow_methods: HeaderValue,
    allow_credentials: HeaderValue,
}

impl Cors {
    pub(crate) fn new(config: Config) -> Cors {
        let expose_headers = join_header_value(&config.exposed_headers).unwrap();
        let allow_methods = HeaderValue::from_static("POST,OPTIONS");
        let allow_credentials = HeaderValue::from_static("true");

        let cache = Arc::new(Cache {
            config,
            expose_headers,
            allow_methods,
            allow_credentials,
        });

        Cors { cache }
    }

    fn is_method_allowed(&self, header: Option<&HeaderValue>) -> bool {
        match header {
            Some(value) => match Method::from_bytes(value.as_bytes()) {
                Ok(method) => DEFAULT_ALLOWED_METHODS.contains(&method),
                Err(_) => {
                    debug!("access-control-request-method {:?} is not valid", value);
                    false
                }
            },
            None => {
                debug!("access-control-request-method is missing");
                false
            }
        }
    }

    pub(crate) fn preflight(
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
        headers.insert(ALLOW_METHODS, self.cache.allow_methods.clone());
        headers.insert(ALLOW_HEADERS, request_headers_header.clone());

        if let Some(max_age) = self.cache.config.max_age {
            headers.insert(MAX_AGE, HeaderValue::from(max_age.as_secs()));
        }

        Ok(headers)
    }

    pub(crate) fn simple(&self, headers: &HeaderMap) -> Result<HeaderMap, Error> {
        match headers.get(header::ORIGIN) {
            Some(origin) if self.is_origin_allowed(origin) => {
                Ok(self.common_headers(origin.clone()))
            }
            Some(_) => Err(Error::OriginNotAllowed),
            None => Ok(HeaderMap::new()),
        }
    }

    fn common_headers(&self, origin: HeaderValue) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(ALLOW_ORIGIN, origin);
        headers.insert(EXPOSE_HEADERS, self.cache.expose_headers.clone());

        if self.cache.config.allow_credentials {
            headers.insert(ALLOW_CREDENTIALS, self.cache.allow_credentials.clone());
        }

        headers
    }

    fn is_origin_allowed(&self, origin: &HeaderValue) -> bool {
        self.cache.config.allowed_origins.is_allowed(origin)
    }

    #[cfg(test)]
    pub(crate) fn __check_preflight(&self, headers: &HeaderMap) -> Result<HeaderMap, Error> {
        self.preflight(
            headers,
            headers.get(ORIGIN).unwrap(),
            headers.get(REQUEST_HEADERS).unwrap(),
        )
    }
}

#[cfg(test)]
impl Default for Cors {
    fn default() -> Self {
        Cors::new(Config::default())
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

    macro_rules! assert_value_eq {
        ($header:expr, $expected:expr) => {
            fn sorted(value: &str) -> Vec<&str> {
                let mut vec = value.split(",").collect::<Vec<_>>();
                vec.sort();
                vec
            }

            assert_eq!(sorted($header.to_str().unwrap()), sorted($expected))
        };
    }

    fn value(s: &str) -> HeaderValue {
        s.parse().unwrap()
    }

    impl From<Config> for Cors {
        fn from(c: Config) -> Self {
            Cors::new(c)
        }
    }

    #[test]
    #[should_panic]
    #[ignore]
    fn origin_is_valid_url() {
        Config::new().allow_origins(vec!["foo"]);
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
            assert_value_eq!(&headers[EXPOSE_HEADERS], "grpc-status,grpc-message");
        }

        #[test]
        fn any_origin() {
            let cors: Cors = Config::new().allow_all_origins().into();

            assert!(cors.__check_preflight(&preflight_headers()).is_ok());
        }

        #[test]
        fn origin_list() {
            let cors: Cors = Config::new()
                .allow_origins(vec![
                    HeaderValue::from_static("http://a.com"),
                    HeaderValue::from_static("http://b.com"),
                ])
                .into();

            let mut req_headers = preflight_headers();
            req_headers.insert(ORIGIN, value("http://b.com"));

            assert!(cors.__check_preflight(&req_headers).is_ok());
        }

        #[test]
        fn origin_not_allowed() {
            let cors: Cors = Config::new().allow_origins(vec!["http://a.com"]).into();

            let err = cors.__check_preflight(&preflight_headers()).unwrap_err();

            assert_eq!(err, Error::OriginNotAllowed)
        }

        #[test]
        fn disallow_credentials() {
            let cors = Cors::new(Config::new().allow_credentials(false));
            let headers = cors.__check_preflight(&preflight_headers()).unwrap();

            assert!(!headers.contains_key(ALLOW_CREDENTIALS));
        }

        #[test]
        fn expose_headers_are_merged() {
            let cors = Cors::new(Config::new().expose_headers(vec!["x-request-id"]));
            let headers = cors.__check_preflight(&preflight_headers()).unwrap();

            assert_value_eq!(
                &headers[EXPOSE_HEADERS],
                "x-request-id,grpc-message,grpc-status"
            );
        }

        #[test]
        fn allow_headers_echo_request_headers() {
            let cors = Cors::default();
            let mut request_headers = preflight_headers();
            request_headers.insert(REQUEST_HEADERS, value("x-grpc-web,foo,x-request-id"));

            let headers = cors.__check_preflight(&request_headers).unwrap();

            assert_value_eq!(&headers[ALLOW_HEADERS], "x-grpc-web,foo,x-request-id");
        }

        #[test]
        fn missing_request_method() {
            let cors = Cors::default();
            let mut request_headers = preflight_headers();
            request_headers.remove(REQUEST_METHOD);

            let err = cors.__check_preflight(&request_headers).unwrap_err();

            assert_eq!(err, Error::MethodNotAllowed);
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

            let cors = Cors::new(Config::new().max_age(Duration::from_secs(99)));
            let headers = cors.__check_preflight(&preflight_headers()).unwrap();

            assert_eq!(headers[MAX_AGE], "99");
        }

        #[test]
        fn no_max_age() {
            let cors = Cors::new(Config::new().max_age(None));
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
            let headers = cors.simple(&request_headers()).unwrap();

            assert_eq!(headers[ALLOW_ORIGIN], "http://example.com");
            assert_eq!(headers[ALLOW_CREDENTIALS], "true");
            assert_value_eq!(&headers[EXPOSE_HEADERS], "grpc-message,grpc-status");

            assert!(!headers.contains_key(ALLOW_HEADERS));
            assert!(!headers.contains_key(ALLOW_METHODS));
            assert!(!headers.contains_key(MAX_AGE));
        }

        #[test]
        fn any_origin() {
            let cors: Cors = Config::new().allow_all_origins().into();

            assert!(cors.simple(&request_headers()).is_ok());
        }

        #[test]
        fn origin_list() {
            let cors: Cors = Config::new()
                .allow_origins(vec![
                    HeaderValue::from_static("http://a.com"),
                    HeaderValue::from_static("http://b.com"),
                ])
                .into();

            let mut req_headers = request_headers();
            req_headers.insert(ORIGIN, value("http://b.com"));

            assert!(cors.simple(&req_headers).is_ok());
        }

        #[test]
        fn origin_not_allowed() {
            let cors: Cors = Config::new().allow_origins(vec!["http://a.com"]).into();

            let err = cors.simple(&request_headers()).unwrap_err();

            assert_eq!(err, Error::OriginNotAllowed)
        }

        #[test]
        fn disallow_credentials() {
            let cors = Cors::new(Config::new().allow_credentials(false));
            let headers = cors.simple(&request_headers()).unwrap();

            assert!(!headers.contains_key(ALLOW_CREDENTIALS));
        }

        #[test]
        fn expose_headers_are_merged() {
            let cors: Cors = Config::new()
                .expose_headers(vec!["x-hello", "custom-1"])
                .into();

            let headers = cors.simple(&request_headers()).unwrap();

            assert_value_eq!(
                &headers[EXPOSE_HEADERS],
                "grpc-message,grpc-status,x-hello,custom-1"
            );
        }
    }
}
