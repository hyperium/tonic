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

#[doc(hidden)]
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

#[doc(hidden)]
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

#[doc(hidden)]
#[derive(Debug)]
pub enum CorsResource {
    Preflight(HeaderMap),
    Simple(HeaderMap),
}

impl Config {
    /// https://www.w3.org/TR/cors/#resource-processing-model
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
