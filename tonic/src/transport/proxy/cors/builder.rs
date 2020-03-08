use super::{AllowedOrigins, Config };
use http::{
    header::{self, HeaderName, HeaderValue},
    Method,
};
use std::{collections::HashSet, time::Duration};

/// Build a configured CORS middleware instance.
#[derive(Debug, Default, Clone)]
pub struct CorsBuilder {
    allowed_methods: HashSet<Method>,
    allowed_origins: AllowedOrigins,
    allowed_headers: HashSet<HeaderName>,
    allow_credentials: bool,
    exposed_headers: HashSet<HeaderName>,
    max_age: Option<Duration>,
    prefer_wildcard: bool,
}

impl CorsBuilder {
    /// Create a new `CorsBuilder` with default configuration.
    ///
    /// By default, all operations are restricted.
    pub fn new() -> CorsBuilder {
        Default::default()
    }

    /// Add origins which are allowed to access this resource
    pub fn allow_origins(mut self, origins: AllowedOrigins) -> Self {
        self.allowed_origins = origins;
        self
    }

    /// Add methods which are allowed to be performed on this resource
    pub fn allow_methods<I>(mut self, methods: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<Method>,
    {
        self.allowed_methods
            .extend(methods.into_iter().map(Into::into));
        self
    }

    /// Add headers which are allowed to be sent to this resource
    pub fn allow_headers<I>(mut self, headers: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<HeaderName>,
    {
        self.allowed_headers
            .extend(headers.into_iter().map(Into::into));
        self
    }

    /// Whether to allow clients to send cookies to this resource or not
    pub fn allow_credentials(mut self, allow_credentials: bool) -> Self {
        self.allow_credentials = allow_credentials;
        self
    }

    /// Add headers which are allowed to be read from the response from this resource
    pub fn expose_headers<I>(mut self, headers: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<HeaderName>,
    {
        self.exposed_headers
            .extend(headers.into_iter().map(Into::into));
        self
    }

    /// Defines the maximum cache lifetime for operations allowed on this
    /// resource
    pub fn max_age(mut self, max_age: Duration) -> Self {
        self.max_age = Some(max_age);
        self
    }

    /// When set, the wildcard ('*') will be used as the value for
    /// AccessControlAllowOrigin. When not set, the incoming origin
    /// will be used.
    ///
    /// If credentials are allowed, the incoming origin will always be
    /// used.
    pub fn prefer_wildcard(mut self, prefer_wildcard: bool) -> Self {
        self.prefer_wildcard = prefer_wildcard;
        self
    }

    pub fn into_config(self) -> Config {
        let Self {
            allow_credentials,
            allowed_headers,
            allowed_methods,
            allowed_origins,
            exposed_headers,
            max_age,
            prefer_wildcard,
        } = self;

        let allowed_headers_header =
            join_header_value(&allowed_headers).expect("Invalid allowed headers");
        let allowed_methods_header =
            join_header_value(&allowed_methods).expect("Invalid allowed methods");
        let exposed_headers_header = if exposed_headers.is_empty() {
            None
        } else {
            Some(join_header_value(&exposed_headers).expect("Invalid exposed headers"))
        };
        let max_age = max_age.map(|v| HeaderValue::from(v.as_secs()));

        let vary_header = join_header_value(&[
            header::ORIGIN,
            header::ACCESS_CONTROL_REQUEST_METHOD,
            header::ACCESS_CONTROL_REQUEST_HEADERS,
        ]).expect("Invalid vary");

        Config {
            allow_credentials,
            allowed_headers,
            allowed_headers_header,
            allowed_methods,
            allowed_methods_header,
            allowed_origins,
            exposed_headers_header,
            max_age,
            prefer_wildcard,
            vary_header,
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
