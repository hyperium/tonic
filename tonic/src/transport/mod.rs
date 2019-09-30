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
//! # use tonic::transport::{Channel, Certificate};
//! # use std::time::Duration;
//! # use tonic::body::BoxBody;
//! # use tonic::client::GrpcService;;
//! # use http::Request;
//! # #[cfg(feature = "rustls")]
//! # async fn do_thing() -> Result<(), Box<dyn std::error::Error>> {
//! let cert = std::fs::read_to_string("ca.pem")?;
//!
//! let mut channel = Channel::from_static("https://example.com")
//!     .rustls_tls(Certificate::from_pem(&cert), "example.com".to_string())
//!     .timeout(Duration::from_secs(5))
//!     .rate_limit(5, Duration::from_secs(1))
//!     .concurrency_limit(256)
//!     .channel();
//!
//! channel.call(Request::new(BoxBody::empty())).await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Server
//!
//! ```no_run
//! # use tonic::transport::{Server, Identity};
//! # use tower::{Service, service_fn};
//! # use futures_util::future::{err, ok};
//! # #[cfg(feature = "rustls")]
//! # async fn do_thing() -> Result<(), Box<dyn std::error::Error>> {
//! # let my_svc = service_fn(|_| ok::<_, tonic::Status>(service_fn(|req| err(tonic::Status::unimplemented("")))));
//! let cert = std::fs::read_to_string("server.pem")?;
//! let key = std::fs::read_to_string("server.key")?;
//!
//! let addr = "[::1]:50051".parse()?;
//!
//! Server::builder()
//!     .rustls_tls(Identity::from_pem(&cert, &key))
//!     .concurrency_limit_per_connection(256)
//!     .interceptor_fn(|svc, req| {
//!         println!("Request: {:?}", req);
//!         svc.call(req)
//!     })
//!     .clone()
//!     .serve(addr, my_svc)
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
pub use self::server::Server;
pub use self::tls::{Certificate, Identity};
pub use hyper::Body;

pub(crate) use self::error::ErrorKind;
