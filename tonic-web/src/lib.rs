//! grpc-web protocol translation for [`tonic`] services.
//!
//! [`tonic_web`] enables tonic servers to handle requests from [grpc-web] clients directly,
//! without the need of an external proxy. It achieves this by wrapping individual tonic services
//! with a [tower] service that performs the translation between protocols and handles `cors`
//! requests.
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
//! ```
//! This will apply a default configuration that works well with grpc-web clients out of the box.
//!
//! You can customize the CORS configuration composing the [`GrpcWebLayer`] with the cors layer of your choice.
//!
//! ```ignore
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let addr = "[::1]:50051".parse().unwrap();
//!     let greeter = GreeterServer::new(MyGreeter::default());
//!
//!     Server::builder()
//!        .accept_http1(true)
//!        // This will apply the gRPC-Web translation layer
//!        .layer(GrpcWebLayer::new())
//!        .add_service(greeter)
//!        .serve(addr)
//!        .await?;
//!
//!    Ok(())
//! }
//! ```
//!
//! Alternatively, if you have a tls enabled server, you could skip setting `accept_http1` to `true`.
//! This works because the browser will handle `ALPN`.
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
#![warn(
    missing_debug_implementations,
    missing_docs,
    rust_2018_idioms,
    unreachable_pub
)]
#![doc(html_root_url = "https://docs.rs/tonic-web/0.11.0")]
#![doc(issue_tracker_base_url = "https://github.com/hyperium/tonic/issues/")]

pub use call::GrpcWebCall;
pub use client::{GrpcWebClientLayer, GrpcWebClientService};
pub use layer::GrpcWebLayer;
pub use service::{GrpcWebService, ResponseFuture};

mod call;
mod client;
mod layer;
mod service;

use http::header::HeaderName;
use std::time::Duration;
use tonic::{body::BoxBody, server::NamedService};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_layer::Layer;
use tower_service::Service;

const DEFAULT_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const DEFAULT_EXPOSED_HEADERS: [&str; 3] =
    ["grpc-status", "grpc-message", "grpc-status-details-bin"];
const DEFAULT_ALLOW_HEADERS: [&str; 4] =
    ["x-grpc-web", "content-type", "x-user-agent", "grpc-timeout"];

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Enable a tonic service to handle grpc-web requests with the default configuration.
///
/// You can customize the CORS configuration composing the [`GrpcWebLayer`] with the cors layer of your choice.
pub fn enable<S>(service: S) -> CorsGrpcWeb<S>
where
    S: Service<http::Request<hyper::Body>, Response = http::Response<BoxBody>>,
    S: Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError> + Send,
{
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::mirror_request())
        .allow_credentials(true)
        .max_age(DEFAULT_MAX_AGE)
        .expose_headers(
            DEFAULT_EXPOSED_HEADERS
                .iter()
                .cloned()
                .map(HeaderName::from_static)
                .collect::<Vec<HeaderName>>(),
        )
        .allow_headers(
            DEFAULT_ALLOW_HEADERS
                .iter()
                .cloned()
                .map(HeaderName::from_static)
                .collect::<Vec<HeaderName>>(),
        );

    tower_layer::layer_fn(|s| CorsGrpcWeb(cors.layer(s))).layer(GrpcWebService::new(service))
}

/// A newtype wrapper around [`GrpcWebLayer`] and [`tower_http::cors::CorsLayer`] to allow
/// `tonic_web::enable` to implement the [`NamedService`] trait.
#[derive(Debug, Clone)]
pub struct CorsGrpcWeb<S>(tower_http::cors::Cors<GrpcWebService<S>>);

impl<S> Service<http::Request<hyper::Body>> for CorsGrpcWeb<S>
where
    S: Service<http::Request<hyper::Body>, Response = http::Response<BoxBody>>,
    S: Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError> + Send,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future =
        <tower_http::cors::Cors<GrpcWebService<S>> as Service<http::Request<hyper::Body>>>::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<hyper::Body>) -> Self::Future {
        self.0.call(req)
    }
}

impl<S> NamedService for CorsGrpcWeb<S>
where
    S: NamedService,
{
    const NAME: &'static str = S::NAME;
}

pub(crate) mod util {
    pub(crate) mod base64 {
        use base64::{
            alphabet,
            engine::{
                general_purpose::{GeneralPurpose, GeneralPurposeConfig},
                DecodePaddingMode,
            },
        };

        pub(crate) const STANDARD: GeneralPurpose = GeneralPurpose::new(
            &alphabet::STANDARD,
            GeneralPurposeConfig::new()
                .with_encode_padding(true)
                .with_decode_padding_mode(DecodePaddingMode::Indifferent),
        );
    }
}
