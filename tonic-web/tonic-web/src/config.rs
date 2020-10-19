use std::collections::{BTreeSet, HashSet};
use std::convert::TryFrom;
use std::time::Duration;

use http::{header::HeaderName, HeaderValue};
use tonic::body::BoxBody;
use tonic::transport::NamedService;
use tower_service::Service;

use crate::service::GrpcWeb;
use crate::{BoxError, BoxFuture};

const DEFAULT_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

const DEFAULT_EXPOSED_HEADERS: [&str; 2] = ["grpc-status", "grpc-message"];

/// Configuration options for wrapped services
///
///  * Default configuration
///  * Not general-purpose CORS, but grpc-web specific
///  * Settings that cannot be changed
///  * Cloning a Config instance
#[derive(Debug, Clone)]
pub struct Config {
    pub(crate) allowed_origins: AllowedOrigins,
    pub(crate) exposed_headers: HashSet<HeaderName>,
    pub(crate) max_age: Option<Duration>,
    pub(crate) allow_credentials: bool,
}

#[derive(Debug, Clone)]
pub(crate) enum AllowedOrigins {
    Any,
    #[allow(clippy::mutable_key_type)]
    Only(BTreeSet<HeaderValue>),
}

impl AllowedOrigins {
    pub(crate) fn is_allowed(&self, origin: &HeaderValue) -> bool {
        match self {
            AllowedOrigins::Any => true,
            AllowedOrigins::Only(origins) => origins.contains(origin),
        }
    }
}

impl Config {
    pub(crate) fn new() -> Config {
        Config {
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

    /// TODO: doc
    pub fn allow_all_origins(self) -> Config {
        Self {
            allowed_origins: AllowedOrigins::Any,
            ..self
        }
    }

    /// TODO: doc
    pub fn allow_origins<I>(self, origins: I) -> Config
    where
        I: IntoIterator,
        HeaderValue: TryFrom<I::Item>,
    {
        // false positive when using HeaderValue, which uses Bytes internally
        // https://rust-lang.github.io/rust-clippy/master/index.html#mutable_key_type
        #[allow(clippy::mutable_key_type)]
        let origins = origins
            .into_iter()
            .map(|v| match TryFrom::try_from(v) {
                Ok(uri) => uri,
                Err(_) => panic!("invalid origin"),
            })
            .collect();

        Self {
            allowed_origins: AllowedOrigins::Only(origins),
            ..self
        }
    }

    /// TODO: doc
    pub fn expose_headers<I>(mut self, headers: I) -> Config
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

    /// TODO: doc
    pub fn max_age<T: Into<Option<Duration>>>(self, max_age: T) -> Config {
        Self {
            max_age: max_age.into(),
            ..self
        }
    }

    /// TODO: doc
    pub fn allow_credentials(self, allow_credentials: bool) -> Config {
        Self {
            allow_credentials,
            ..self
        }
    }

    /// TODO: doc, return type
    pub fn enable<S>(
        &self,
        service: S,
    ) -> impl Service<
        http::Request<hyper::Body>,
        Response = http::Response<BoxBody>,
        Error = S::Error,
        Future = BoxFuture<S::Response, S::Error>,
    > + NamedService
           + Clone
    where
        S: Service<http::Request<hyper::Body>, Response = http::Response<BoxBody>>,
        S: NamedService + Clone + Send + 'static,
        S::Future: Send + 'static,
        S::Error: Into<BoxError> + Send,
    {
        tracing::trace!("enabled for {}", S::NAME);
        GrpcWeb::new(service, self.clone())
    }
}

impl Default for Config {
    fn default() -> Self {
        Config::new()
    }
}
