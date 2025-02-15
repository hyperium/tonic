//! grpc-web protocol translation for [`tonic`] services.
//!
//! [`tonic_web`] enables tonic servers to handle requests from [grpc-web] clients directly,
//! without the need of an external proxy. It achieves this by wrapping individual tonic services
//! with a [tower] service that performs the translation between protocols and handles `cors`
//! requests.
//!
//! ## Enabling tonic services
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
//!        .layer(GrpcWebLayer::new())
//!        .add_service(greeter)
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
//!   handle arbitrary HTTP/x.x requests or bespoke protocols.
//! * Similarly, the cors support implemented  by this crate will *only* handle grpc-web and
//!   grpc-web preflight requests.
//! * Currently, grpc-web clients can only perform `unary` and `server-streaming` calls. These
//!   are the only requests this crate is designed to handle. Support for client and bi-directional
//!   streaming will be officially supported when clients do.
//! * There is no support for web socket transports.
//!
//!
//! [`tonic`]: https://github.com/hyperium/tonic
//! [`tonic_web`]: https://github.com/hyperium/tonic
//! [grpc-web]: https://github.com/grpc/grpc-web
//! [tower]: https://github.com/tower-rs/tower
#![doc(issue_tracker_base_url = "https://github.com/hyperium/tonic/issues/")]

pub use call::GrpcWebCall;
pub use client::{GrpcWebClientLayer, GrpcWebClientService};
pub use layer::GrpcWebLayer;
pub use service::{GrpcWebService, ResponseFuture};

mod call;
mod client;
mod layer;
mod service;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

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
