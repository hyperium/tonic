//! TODO: doc
#![warn(
    missing_debug_implementations,
    missing_docs,
    rust_2018_idioms,
    unreachable_pub
)]

pub use config::Config;

mod call;
mod config;
mod cors;
mod service;

use crate::service::GrpcWeb;
use std::future::Future;
use std::pin::Pin;
use tonic::body::BoxBody;
use tonic::transport::NamedService;
use tower_service::Service;

/// TODO: doc, return type
pub fn enable<S>(
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
    enable_with_config(service, Config::default())
}

/// TODO: doc, return type
pub fn enable_with_config<S>(
    service: S,
    config: Config,
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
    GrpcWeb::new(service, config)
}

/// TODO: doc
pub fn config() -> Config {
    Config::default()
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type BoxFuture<T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send>>;
