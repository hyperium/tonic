//! Batteries included server and client.
//!
//! This module provides a set of batteries included, fully featured and
//! fast set of HTTP/2 server and client's. These components each provide either an
//! `openssl` or `rustls` tls backend when the respective feature flags are enabled.
//!They also provide may configurable knobs that can be used to tune how they work.
//!
//! # Features
//!
//! - TLS support via either [OpenSSL] or [rustls].
//! - Load balancing
//! - Timeouts
//! - Concurrency Limits
//! - Rate limiting
//! - gRPC Interceptors
//!
//! # Examples
//!
//! ## Client
//!
//! ```no_run
//! # use tonic::transport::{Channel, Certificate, ClientTlsConfig};
//! # use std::time::Duration;
//! # use tonic::body::BoxBody;
//! # use tonic::client::GrpcService;;
//! # use http::Request;
//! # #[cfg(feature = "rustls")]
//! # async fn do_thing() -> Result<(), Box<dyn std::error::Error>> {
//! let cert = std::fs::read_to_string("ca.pem")?;
//!
//! let mut channel = Channel::from_static("https://example.com")
//!     .tls_config(ClientTlsConfig::with_rustls()
//!         .ca_certificate(Certificate::from_pem(&cert))
//!         .domain_name("example.com".to_string()))
//!     .timeout(Duration::from_secs(5))
//!     .rate_limit(5, Duration::from_secs(1))
//!     .concurrency_limit(256)
//!     .connect()
//!     .await?;
//!
//! channel.call(Request::new(BoxBody::empty())).await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Server
//!
//! ```no_run
//! # use tonic::transport::{Server, Identity, ServerTlsConfig};
//! # use tower::{Service, service_fn};
//! # use futures_util::future::{err, ok};
//! # #[cfg(feature = "rustls")]
//! # async fn do_thing() -> Result<(), Box<dyn std::error::Error>> {
//! # #[derive(Clone)]
//! # pub struct Svc;
//! # impl Service<hyper::Request<hyper::Body>> for Svc {
//! #   type Response = hyper::Response<tonic::body::BoxBody>;
//! #   type Error = tonic::Status;
//! #   type Future = futures_util::future::Ready<Result<Self::Response, Self::Error>>;
//! #   fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
//! #       Ok(()).into()
//! #  }
//! #   fn call(&mut self, _req: hyper::Request<hyper::Body>) -> Self::Future {
//! #       unimplemented!()
//! #   }
//! # }
//! # impl tonic::transport::ServiceName for Svc {
//! # const NAME: &'static str = "some_svc";
//! # }
//! # let my_svc = Svc;
//! let cert = std::fs::read_to_string("server.pem")?;
//! let key = std::fs::read_to_string("server.key")?;
//!
//! let addr = "[::1]:50051".parse()?;
//!
//! Server::builder()
//!     .tls_config(ServerTlsConfig::with_rustls()
//!         .identity(Identity::from_pem(&cert, &key)))
//!     .concurrency_limit_per_connection(256)
//!     .interceptor_fn(|svc, req| {
//!         println!("Request: {:?}", req);
//!         svc.call(req)
//!     })
//!     .add_service(my_svc)
//!     .serve(addr)
//!     .await?;
//!
//! # Ok(())
//! # }
//! ```
//!
//! [OpenSSL]: https://www.openssl.org/
//! [rustls]: https://docs.rs/rustls/0.16.0/rustls/

pub mod channel;
pub mod server;

mod endpoint;
mod error;
mod service;
mod tls;

#[doc(inline)]
pub use self::channel::Channel;
pub use self::endpoint::Endpoint;
pub use self::error::Error;
#[doc(inline)]
pub use self::server::{Server, ServiceName};
pub use self::tls::{Certificate, Identity};
pub use hyper::Body;

#[cfg(feature = "tls")]
pub use self::endpoint::ClientTlsConfig;
#[cfg(feature = "tls")]
pub use self::server::ServerTlsConfig;

pub(crate) use self::error::ErrorKind;
