use http::{
    header::{self, HeaderMap, HeaderName, HeaderValue},
    method, Method, Request,
};
use std::{
    collections::{BTreeSet, HashSet},
    error, fmt,
    iter::FromIterator,
};

/// Specifies which origins are allowed to access this resource
#[derive(Debug, Clone)]
pub enum AllowedOrigins {
    /// Any origin is allowed
    Any {
        /// Allowing a null origin is a separate setting, since it's
        /// risky to trust sources with a null origin, see
        /// https://tools.ietf.org/id/draft-abarth-origin-03.html#rfc.section.6
        /// https://w3c.github.io/webappsec-cors-for-developers/
        allow_null: bool,
    },

    /// Allow a specific set of origins
    Origins(BTreeSet<HeaderValue>),
}

impl AllowedOrigins {
    fn origin_allowed(&self, origin: &HeaderValue) -> bool {
        match self {
            AllowedOrigins::Any { allow_null } => {
                *allow_null || origin != HeaderValue::from_static("null")
            }
            AllowedOrigins::Origins(origins) => origins.contains(origin),
        }
    }
}

impl Default for AllowedOrigins {
    fn default() -> Self {
        AllowedOrigins::Origins(Default::default())
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

#[derive(Debug)]
pub enum InvalidRequest {
    DisallowedOrigin,
    InvalidMethod(method::InvalidMethod),
    DisallowedMethod,
    InvalidHeader(header::InvalidHeaderName),
    DisallowedHeader,
}

impl error::Error for InvalidRequest {
    fn description(&self) -> &str {
        "description() is deprecated; use Display"
    }
}

impl fmt::Display for InvalidRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub allowed_methods: HashSet<Method>,
    pub allowed_methods_header: HeaderValue,
    pub allowed_origins: AllowedOrigins,
    pub allowed_headers: HashSet<HeaderName>,
    pub allowed_headers_header: HeaderValue,
    pub allow_credentials: bool,
    pub exposed_headers_header: Option<HeaderValue>,
    pub max_age: Option<HeaderValue>,
    pub prefer_wildcard: bool,
    pub vary_header: HeaderValue,
}

#[derive(Debug)]
pub enum CorsResource {
    Preflight(HeaderMap),
    Simple(HeaderMap),
}

impl Config {
    // https://www.w3.org/TR/cors/#resource-processing-model
    pub fn process_request<B>(&self, request: &Request<B>) -> Result<CorsResource, InvalidRequest> {
        use self::InvalidRequest::*;

        let origin = request.headers().get(header::ORIGIN);
        let requested_method = request.headers().get(header::ACCESS_CONTROL_REQUEST_METHOD);

        match (origin, request.method(), requested_method) {
            (None, _, _) => {
                // Without an origin, this cannot be a CORS request
                let headers = self.basic_headers();
                Ok(CorsResource::Simple(headers))
            }
            (Some(origin), &Method::OPTIONS, Some(requested_method)) => {
                // Preflight request
                // https://www.w3.org/TR/cors/#resource-preflight-requests

                if !self.allowed_origins.origin_allowed(&origin) {
                    return Err(DisallowedOrigin);
                }

                let requested_method =
                    Method::from_bytes(requested_method.as_bytes()).map_err(InvalidMethod)?;

                if !self.allowed_methods.contains(&requested_method) {
                    return Err(DisallowedMethod);
                }

                let requested_headers: Result<HashSet<_>, _> = match request
                    .headers()
                    .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
                {
                    Some(headers) => headers
                        .as_bytes()
                        .split(|&b| b == b',')
                        .map(HeaderName::from_bytes)
                        .collect(),
                    None => Ok(Default::default()),
                };

                let requested_headers = requested_headers.map_err(InvalidHeader)?;

                let mut invalid_headers = requested_headers.difference(&self.allowed_headers);

                if invalid_headers.next().is_some() {
                    return Err(DisallowedHeader);
                }

                // All checks complete; generate response

                let mut headers = self.common_headers(origin.clone());

                headers.insert(
                    header::ACCESS_CONTROL_ALLOW_METHODS,
                    self.allowed_methods_header.clone(),
                );

                headers.insert(
                    header::ACCESS_CONTROL_ALLOW_HEADERS,
                    self.allowed_headers_header.clone(),
                );

                if let Some(ref max_age) = self.max_age {
                    headers.insert(header::ACCESS_CONTROL_MAX_AGE, max_age.clone());
                }

                Ok(CorsResource::Preflight(headers))
            }
            (Some(origin), _, _) => {
                // Simple / Actual request
                // https://www.w3.org/TR/cors/#resource-requests

                if !self.allowed_origins.origin_allowed(&origin) {
                    return Err(DisallowedOrigin);
                }

                let mut headers = self.common_headers(origin.clone());

                if let Some(ref exposed_headers) = self.exposed_headers_header {
                    headers.insert(
                        header::ACCESS_CONTROL_EXPOSE_HEADERS,
                        exposed_headers.clone(),
                    );
                }

                Ok(CorsResource::Simple(headers))
            }
        }
    }

    fn common_headers(&self, origin: HeaderValue) -> HeaderMap {
        let mut headers = self.basic_headers();

        if self.allow_credentials {
            headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);

            headers.insert(
                header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
                HeaderValue::from_static("true"),
            );
        } else {
            let allowed_origin = if self.prefer_wildcard {
                HeaderValue::from_static("*")
            } else {
                origin
            };
            headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, allowed_origin);
        }

        headers
    }

    fn basic_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::VARY, self.vary_header.clone());
        headers
    }
}

/*

#[cfg(test)]
mod test {
    use http;
    use std::time::Duration;

    use crate::cors::CorsBuilder;

    use self::InvalidRequest::*;
    use super::*;

    type TestError = Box<dyn (::std::error::Error)>;
    type TestResult<T = ()> = ::std::result::Result<T, TestError>;

    macro_rules! assert_variant {
        ($value:expr, $var:pat) => {
            match $value {
                $var => {}
                _ => assert!(
                    false,
                    "Expected variant {}, was {:?}",
                    stringify!($var),
                    $value
                ),
            }
        };
    }

    macro_rules! assert_set {
        ($header:expr, $($val:expr),+) => {
            let actual = BTreeSet::from_iter($header.to_str()?.split(","));
            let expected = BTreeSet::from_iter(vec![$($val),+]);
            assert_eq!(actual, expected);
        }
    }

    impl CorsResource {
        fn into_simple(self) -> TestResult<HeaderMap> {
            match self {
                CorsResource::Simple(h) => Ok(h),
                _ => Err("Not a simple resource".into()),
            }
        }

        fn into_preflight(self) -> TestResult<HeaderMap> {
            match self {
                CorsResource::Preflight(h) => Ok(h),
                _ => Err("Not a preflight resource".into()),
            }
        }
    }

    #[test]
    fn simple_allows_when_origin_is_any() -> TestResult {
        common_allows_when_origin_is_any(
            simple_origin_config_builder(),
            simple_origin_request_builder,
        )
    }

    #[test]
    fn simple_disallows_null_origin_even_for_any() -> TestResult {
        common_disallows_null_origin_even_for_any(
            simple_origin_config_builder(),
            simple_origin_request_builder,
        )
    }

    #[test]
    fn simple_allows_null_origin_for_any_when_configured() -> TestResult {
        common_allows_null_origin_for_any_when_configured(
            simple_origin_config_builder(),
            simple_origin_request_builder,
        )
    }

    #[test]
    fn simple_compares_origin_against_allowed_origins() -> TestResult {
        common_compares_origin_against_allowed_origins(
            simple_origin_config_builder(),
            simple_origin_request_builder,
        )
    }

    fn simple_origin_config_builder() -> CorsBuilder {
        CorsBuilder::new()
    }

    fn simple_origin_request_builder() -> TestResult<http::request::Builder> {
        Ok(http::Request::builder())
    }

    #[test]
    fn simple_response_includes_vary_header() -> TestResult {
        let builder = CorsBuilder::new()
            .allow_origins(AllowedOrigins::Any { allow_null: false })
            .allow_methods(vec![Method::POST]);

        let req = http::Request::builder()
            .header(
                header::ORIGIN,
                HeaderValue::from_static("http://test.example"),
            ).body(())?;

        common_test_vary_header(builder, req, CorsResource::into_simple)
    }

    #[test]
    fn simple_response_includes_allowed_credentials() -> TestResult {
        let builder = CorsBuilder::new()
            .allow_origins(AllowedOrigins::Any { allow_null: false })
            .allow_methods(vec![Method::POST]);

        let req = http::Request::builder()
            .header(
                header::ORIGIN,
                HeaderValue::from_static("http://test.example"),
            ).body(())?;

        common_test_allowed_credentials(builder, req, CorsResource::into_simple)
    }

    #[test]
    fn simple_response_includes_allowed_origin() -> TestResult {
        let builder = CorsBuilder::new()
            .allow_origins(AllowedOrigins::Any { allow_null: false })
            .allow_methods(vec![Method::POST]);

        let req = http::Request::builder()
            .header(
                header::ORIGIN,
                HeaderValue::from_static("http://test.example"),
            ).body(())?;

        common_test_allowed_origin(builder, req, CorsResource::into_simple)
    }

    #[test]
    fn simple_response_includes_exposed_headers() -> TestResult {
        let cfg = CorsBuilder::new()
            .allow_origins(AllowedOrigins::Any { allow_null: false })
            .allow_methods(vec![Method::POST])
            .expose_headers(&[header::WARNING, HeaderName::from_static("x-custom")])
            .into_config();

        let req = http::Request::builder()
            .header(
                header::ORIGIN,
                HeaderValue::from_static("http://test.example"),
            ).body(())?;

        let mut headers = cfg.process_request(&req)?.into_simple()?;
        let hdr = headers
            .remove(header::ACCESS_CONTROL_EXPOSE_HEADERS)
            .expect("expose-headers header missing");

        assert_set!(hdr, "warning", "x-custom");

        Ok(())
    }

    #[test]
    fn preflight_allows_when_origin_is_any() -> TestResult {
        common_allows_when_origin_is_any(
            preflight_origin_config_builder(),
            preflight_origin_request_builder,
        )
    }

    #[test]
    fn preflight_disallows_null_origin_even_for_any() -> TestResult {
        common_disallows_null_origin_even_for_any(
            preflight_origin_config_builder(),
            preflight_origin_request_builder,
        )
    }

    #[test]
    fn preflight_allows_null_origin_for_any_when_configured() -> TestResult {
        common_allows_null_origin_for_any_when_configured(
            preflight_origin_config_builder(),
            preflight_origin_request_builder,
        )
    }

    #[test]
    fn preflight_compares_origin_against_allowed_origins() -> TestResult {
        common_compares_origin_against_allowed_origins(
            preflight_origin_config_builder(),
            preflight_origin_request_builder,
        )
    }

    fn preflight_origin_config_builder() -> CorsBuilder {
        CorsBuilder::new().allow_methods(vec![Method::POST])
    }

    fn preflight_origin_request_builder() -> TestResult<http::request::Builder> {
        let mut builder = http::Request::builder();
        builder.method(Method::OPTIONS).header(
            header::ACCESS_CONTROL_REQUEST_METHOD,
            HeaderValue::from_static("POST"),
        );
        Ok(builder)
    }

    #[test]
    fn preflight_compares_method_against_allowed_methods() -> TestResult {
        let cfg = CorsBuilder::new()
            .allow_origins(AllowedOrigins::Any { allow_null: false })
            .allow_methods(vec![Method::POST, Method::PATCH])
            .into_config();

        let builder = || -> TestResult<http::request::Builder> {
            let mut builder = http::Request::builder();
            builder.method(Method::OPTIONS).header(
                header::ORIGIN,
                HeaderValue::from_static("http://test.example"),
            );
            Ok(builder)
        };

        let allowed_req_post = builder()?
            .header(
                header::ACCESS_CONTROL_REQUEST_METHOD,
                HeaderValue::from_static("POST"),
            ).body(())?;

        assert_variant!(cfg.process_request(&allowed_req_post), Ok(_));

        let allowed_req_patch = builder()?
            .header(
                header::ACCESS_CONTROL_REQUEST_METHOD,
                HeaderValue::from_static("PATCH"),
            ).body(())?;

        assert_variant!(cfg.process_request(&allowed_req_patch), Ok(_));

        let disallowed_req_put = builder()?
            .header(
                header::ACCESS_CONTROL_REQUEST_METHOD,
                HeaderValue::from_static("PUT"),
            ).body(())?;

        assert_variant!(
            cfg.process_request(&disallowed_req_put),
            Err(DisallowedMethod)
        );

        Ok(())
    }

    #[test]
    fn preflight_compares_headers_against_allowed_headers() -> TestResult {
        let cfg = CorsBuilder::new()
            .allow_origins(AllowedOrigins::Any { allow_null: false })
            .allow_methods(vec![Method::POST])
            .allow_headers(&[
                header::SERVER,
                header::WARNING,
                HeaderName::from_static("x-custom"),
            ]).into_config();

        let builder = || -> TestResult<http::request::Builder> {
            let mut builder = http::Request::builder();
            builder
                .method(Method::OPTIONS)
                .header(
                    header::ORIGIN,
                    HeaderValue::from_static("http://test.example"),
                ).header(
                    header::ACCESS_CONTROL_REQUEST_METHOD,
                    HeaderValue::from_static("POST"),
                );
            Ok(builder)
        };

        let allowed_req_server = builder()?
            .header(
                header::ACCESS_CONTROL_REQUEST_HEADERS,
                HeaderValue::from(header::SERVER),
            ).body(())?;

        assert_variant!(cfg.process_request(&allowed_req_server), Ok(_));

        let allowed_req_warning = builder()?
            .header(
                header::ACCESS_CONTROL_REQUEST_HEADERS,
                HeaderValue::from(header::WARNING),
            ).body(())?;

        assert_variant!(cfg.process_request(&allowed_req_warning), Ok(_));

        let allowed_req_multiple = builder()?
            .header(
                header::ACCESS_CONTROL_REQUEST_HEADERS,
                HeaderValue::from_static("server,warning,x-custom"),
            ).body(())?;

        assert_variant!(cfg.process_request(&allowed_req_multiple), Ok(_));

        let allowed_req_differing_case = builder()?
            .header(
                header::ACCESS_CONTROL_REQUEST_HEADERS,
                HeaderValue::from_static("Server,WARNING,X-cUsToM"),
            ).body(())?;

        assert_variant!(cfg.process_request(&allowed_req_differing_case), Ok(_));

        let disallowed_req_range = builder()?
            .header(
                header::ACCESS_CONTROL_REQUEST_HEADERS,
                HeaderValue::from(header::CONTENT_RANGE),
            ).body(())?;

        assert_variant!(
            cfg.process_request(&disallowed_req_range),
            Err(DisallowedHeader)
        );

        Ok(())
    }

    #[test]
    fn preflight_response_includes_vary_header() -> TestResult {
        let builder = CorsBuilder::new()
            .allow_origins(AllowedOrigins::Any { allow_null: false })
            .allow_methods(vec![Method::POST]);

        let req = http::Request::builder()
            .method(Method::OPTIONS)
            .header(
                header::ORIGIN,
                HeaderValue::from_static("http://test.example"),
            ).header(
                header::ACCESS_CONTROL_REQUEST_METHOD,
                HeaderValue::from_static("POST"),
            ).body(())?;

        common_test_vary_header(builder, req, CorsResource::into_preflight)
    }

    #[test]
    fn preflight_response_includes_allowed_credentials() -> TestResult {
        let builder = CorsBuilder::new()
            .allow_origins(AllowedOrigins::Any { allow_null: false })
            .allow_methods(vec![Method::POST]);

        let req = http::Request::builder()
            .method(Method::OPTIONS)
            .header(
                header::ORIGIN,
                HeaderValue::from_static("http://test.example"),
            ).header(
                header::ACCESS_CONTROL_REQUEST_METHOD,
                HeaderValue::from_static("POST"),
            ).body(())?;

        common_test_allowed_credentials(builder, req, CorsResource::into_preflight)
    }

    #[test]
    fn preflight_response_includes_allowed_origin() -> TestResult {
        let builder = CorsBuilder::new()
            .allow_origins(AllowedOrigins::Any { allow_null: false })
            .allow_methods(vec![Method::POST]);

        let req = http::Request::builder()
            .method(Method::OPTIONS)
            .header(
                header::ORIGIN,
                HeaderValue::from_static("http://test.example"),
            ).header(
                header::ACCESS_CONTROL_REQUEST_METHOD,
                HeaderValue::from_static("POST"),
            ).body(())?;

        common_test_allowed_origin(builder, req, CorsResource::into_preflight)
    }

    #[test]
    fn preflight_response_includes_allowed_methods() -> TestResult {
        let cfg = CorsBuilder::new()
            .allow_origins(AllowedOrigins::Any { allow_null: false })
            .allow_methods(vec![
                Method::POST,
                Method::PATCH,
                Method::from_bytes(b"LIST")?,
            ]).into_config();

        let req = http::Request::builder()
            .method(Method::OPTIONS)
            .header(
                header::ORIGIN,
                HeaderValue::from_static("http://test.example"),
            ).header(
                header::ACCESS_CONTROL_REQUEST_METHOD,
                HeaderValue::from_static("POST"),
            ).body(())?;

        let mut headers = cfg.process_request(&req)?.into_preflight()?;
        let hdr = headers
            .remove(header::ACCESS_CONTROL_ALLOW_METHODS)
            .expect("allow-methods header missing");

        assert_set!(hdr, "PATCH", "LIST", "POST");

        Ok(())
    }

    #[test]
    fn preflight_response_includes_allowed_headers() -> TestResult {
        let cfg = CorsBuilder::new()
            .allow_origins(AllowedOrigins::Any { allow_null: false })
            .allow_methods(vec![Method::POST])
            .allow_headers(&[
                header::SERVER,
                header::WARNING,
                HeaderName::from_static("x-custom"),
            ]).into_config();

        let req = http::Request::builder()
            .method(Method::OPTIONS)
            .header(
                header::ORIGIN,
                HeaderValue::from_static("http://test.example"),
            ).header(
                header::ACCESS_CONTROL_REQUEST_METHOD,
                HeaderValue::from_static("POST"),
            ).body(())?;

        let mut headers = cfg.process_request(&req)?.into_preflight()?;
        let hdr = headers
            .remove(header::ACCESS_CONTROL_ALLOW_HEADERS)
            .expect("allow-headers header missing");

        assert_set!(hdr, "server", "warning", "x-custom");

        Ok(())
    }

    #[test]
    fn preflight_response_includes_max_age() -> TestResult {
        let cfg = CorsBuilder::new()
            .allow_origins(AllowedOrigins::Any { allow_null: false })
            .allow_methods(vec![Method::POST])
            .max_age(Duration::from_secs(42))
            .into_config();

        let req = http::Request::builder()
            .method(Method::OPTIONS)
            .header(
                header::ORIGIN,
                HeaderValue::from_static("http://test.example"),
            ).header(
                header::ACCESS_CONTROL_REQUEST_METHOD,
                HeaderValue::from_static("POST"),
            ).body(())?;

        let mut headers = cfg.process_request(&req)?.into_preflight()?;
        let hdr = headers
            .remove(header::ACCESS_CONTROL_MAX_AGE)
            .expect("max-age header missing");

        assert_eq!(hdr, "42");

        Ok(())
    }

    fn common_allows_when_origin_is_any(
        cfg_builder: CorsBuilder,
        req_builder: impl Fn() -> TestResult<http::request::Builder>,
    ) -> TestResult {
        let cfg = cfg_builder
            .allow_origins(AllowedOrigins::Any { allow_null: false })
            .into_config();

        let req = req_builder()?
            .header(
                header::ORIGIN,
                HeaderValue::from_static("http://test.example"),
            ).body(())?;

        assert_variant!(cfg.process_request(&req), Ok(_));

        Ok(())
    }

    fn common_disallows_null_origin_even_for_any(
        cfg_builder: CorsBuilder,
        req_builder: impl Fn() -> TestResult<http::request::Builder>,
    ) -> TestResult {
        let cfg = cfg_builder
            .allow_origins(AllowedOrigins::Any { allow_null: false })
            .into_config();

        let req = req_builder()?
            .header(header::ORIGIN, HeaderValue::from_static("null"))
            .body(())?;

        assert_variant!(cfg.process_request(&req), Err(DisallowedOrigin));

        Ok(())
    }

    fn common_allows_null_origin_for_any_when_configured(
        cfg_builder: CorsBuilder,
        req_builder: impl Fn() -> TestResult<http::request::Builder>,
    ) -> TestResult {
        let cfg = cfg_builder
            .allow_origins(AllowedOrigins::Any { allow_null: true })
            .into_config();

        let req = req_builder()?
            .header(header::ORIGIN, HeaderValue::from_static("null"))
            .body(())?;

        assert_variant!(cfg.process_request(&req), Ok(_));

        Ok(())
    }

    fn common_compares_origin_against_allowed_origins(
        cfg_builder: CorsBuilder,
        req_builder: impl Fn() -> TestResult<http::request::Builder>,
    ) -> TestResult {
        let cfg = cfg_builder
            .allow_origins(AllowedOrigins::from_iter(vec![
                HeaderValue::from_static("http://foo.example"),
                HeaderValue::from_static("http://bar.example"),
            ])).into_config();

        let allowed_req_foo = req_builder()?
            .header(
                header::ORIGIN,
                HeaderValue::from_static("http://foo.example"),
            ).body(())?;

        assert_variant!(cfg.process_request(&allowed_req_foo), Ok(_));

        let allowed_req_bar = req_builder()?
            .header(
                header::ORIGIN,
                HeaderValue::from_static("http://bar.example"),
            ).body(())?;

        assert_variant!(cfg.process_request(&allowed_req_bar), Ok(_));

        let disallowed_req_unlisted = req_builder()?
            .header(
                header::ORIGIN,
                HeaderValue::from_static("http://quux.example"),
            ).body(())?;

        assert_variant!(
            cfg.process_request(&disallowed_req_unlisted),
            Err(DisallowedOrigin)
        );

        let disallowed_req_differing_case = req_builder()?
            .header(
                header::ORIGIN,
                HeaderValue::from_static("http://FOO.example"),
            ).body(())?;

        assert_variant!(
            cfg.process_request(&disallowed_req_differing_case),
            Err(DisallowedOrigin)
        );

        let disallowed_req_differing_scheme = req_builder()?
            .header(
                header::ORIGIN,
                HeaderValue::from_static("https://foo.example"),
            ).body(())?;

        assert_variant!(
            cfg.process_request(&disallowed_req_differing_scheme),
            Err(DisallowedOrigin)
        );

        Ok(())
    }

    fn common_test_vary_header<B>(
        builder: CorsBuilder,
        req: http::Request<B>,
        f: impl Fn(CorsResource) -> TestResult<HeaderMap>,
    ) -> TestResult {
        let cfg = builder.into_config();

        let mut headers = f(cfg.process_request(&req)?)?;

        let hdr = headers.remove(header::VARY).expect("vary header missing");

        assert_set!(
            hdr,
            "origin",
            "access-control-request-method",
            "access-control-request-headers"
        );

        Ok(())
    }

    fn common_test_allowed_credentials<B>(
        builder: CorsBuilder,
        req: http::Request<B>,
        f: impl Fn(CorsResource) -> TestResult<HeaderMap>,
    ) -> TestResult {
        let cfg = builder.allow_credentials(true).into_config();

        let mut headers = f(cfg.process_request(&req)?)?;
        let hdr = headers
            .remove(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
            .expect("allow-credentials header missing");

        assert_eq!(hdr, "true");

        Ok(())
    }

    fn common_test_allowed_origin<B>(
        builder: CorsBuilder,
        req: http::Request<B>,
        f: impl Fn(CorsResource) -> TestResult<HeaderMap>,
    ) -> TestResult {
        let cfg_no_wildcard_no_credentials = builder.clone().into_config();
        let mut headers = f(cfg_no_wildcard_no_credentials.process_request(&req)?)?;
        let hdr = headers
            .remove(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .expect("allow-origin header missing");

        assert_eq!(hdr, "http://test.example");

        let cfg_wildcard_no_credentials = builder.clone().prefer_wildcard(true).into_config();
        let mut headers = f(cfg_wildcard_no_credentials.process_request(&req)?)?;
        let hdr = headers
            .remove(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .expect("allow-origin header missing");

        assert_eq!(hdr, "*");

        let cfg_wildcard_credentials = builder
            .clone()
            .prefer_wildcard(true)
            .allow_credentials(true)
            .into_config();
        let mut headers = f(cfg_wildcard_credentials.process_request(&req)?)?;
        let hdr = headers
            .remove(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .expect("allow-origin header missing");

        assert_eq!(hdr, "http://test.example");

        Ok(())
    }
}
*/