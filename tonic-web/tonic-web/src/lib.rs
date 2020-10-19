//! grpc-WEB protocol translation for Tonic services.
//!
//! This crate provides a wrapper to decorate tonic services...
//!
//!  * grpc-WEB requests
//!  * grpc-WEB preflight requests
//!  * http1 requests
//!
//!  ## Configuring Tonic
//!
//!  * easiest: `accept_http1` setting
//!  * preferred: ALPN
//!  
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

use std::future::Future;
use std::pin::Pin;
use tonic::body::BoxBody;
use tonic::transport::NamedService;
use tower_service::Service;

// TODO: improve placeholder docs, `enable` return type

/// enable a tonic service to accept grpc-WEB requests, applying configuration.
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
    config().enable(service)
}

/// returns an instance of `Config`
pub fn config() -> Config {
    Config::default()
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type BoxFuture<T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send>>;
