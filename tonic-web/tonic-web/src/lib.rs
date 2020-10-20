//! grpc-web protocol translation for [`tonic`] services.
//!
//! [`tonic_web`] enables tonic servers to handle requests from [grpc-web] clients directly,
//! without the need of an external proxy. It achieves this by wrapping individual tonic services
//! with a [tower] service that performs the translation between protocols and handles `cors`
//! requests.
//!
//! ## Getting Started
//!
//! ```toml
//! [dependencies]
//! tonic_web = { version = "0.1" }
//! ```
//!
//! ## Enabling tonic services
//!
//! The easiest way to get started, is to call the [`enable`] function with your tonic service
//! and allow the tonic server to accept HTTP/1.1 requests:
//!
//! ```ignore
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let addr = "[::1]:50051".parse().unwrap();
//!     let greeter = GreeterServer::new(MyGreeter::default());
//!
//!     Server::builder()
//!        .accept_http1(true)
//!        .add_service(tonic_web::enable(greeter))
//!        .serve(addr)
//!        .await?;
//!
//!    Ok(())
//! }
//!
//! ```
//! This will apply a default configuration that works well with grpc-web clients out of the box.
//! See the [`Config`] documentation for details.
//!
//! Alternatively, if you have a tls enabled server, there is no need for the server to accept
//! HTTP/1.1 requests:
//!
//! ```ignore
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let cert = tokio::fs::read("server.pem").await?;
//!     let key = tokio::fs::read("server.key").await?;
//!     let identity = Identity::from_pem(cert, key);
//!
//!     let addr = "[::1]:50051".parse().unwrap();
//!     let greeter = GreeterServer::new(MyGreeter::default());
//!
//!     // No need to enable HTTP/1
//!     Server::builder()
//!        .tls_config(ServerTlsConfig::new().identity(identity))?
//!        .add_service(tonic_web::enable(greeter))
//!        .serve(addr)
//!        .await?;
//!
//!    Ok(())
//! }
//! ```
//! This works because the browser will handle `ALPN`.
//!
//! ## Limitations
//!
//! * `tonic_web` is designed to work with grpc-web-compliant clients only. It is not expected to
//! handle arbitrary HTTP/x.x requests or bespoke protocols.
//! * Similarly, the cors support implemented  by this crate will *only* handle grpc-web and
//! grpc-web preflight requests.
//! * Currently, grpc-web clients can only perform `unary` and `server-streaming` calls. These
//! are the only requests this crate is designed to handle. Support for client and bi-directional
//! streaming will be officially supported when clients do.
//! * There is no support for web socket transports.
//!
//!
//! [`tonic`]: https://github.com/hyperium/tonic
//! [`tonic_web`]: https://github.com/hyperium/tonic
//! [grpc-web]: https://github.com/grpc/grpc-web
//! [tower]: https://github.com/tower-rs/tower
//! [`enable`]: crate::enable()
//! [`Config`]: crate::Config
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

/// enable a tonic service to handle grpc-web requests with the default configuration.
///
/// Shortcut for `tonic_web::config().enable(service)`
pub fn enable<S>(service: S) -> GrpcWeb<S>
where
    S: Service<http::Request<hyper::Body>, Response = http::Response<BoxBody>>,
    S: NamedService + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError> + Send,
{
    config().enable(service)
}

/// returns a default `Config` instance for configuring services.
///
/// ## Example
///
/// ```
/// let config = tonic_web::config()
///      .allow_origins(vec!["http://foo.com"])
///      .allow_credentials(false)
///      .expose_headers(vec!["x-request-id"]);
///
/// // let greeter = config.enable(Greeter);
/// // let route_guide = config.enable(RouteGuide);
/// ```
pub fn config() -> Config {
    Config::default()
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type BoxFuture<T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send>>;
