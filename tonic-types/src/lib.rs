//! A collection of useful protobuf types that can be used with `tonic`.
//!
//! This crate also introduces the [`StatusExt`] trait and implements it in
//! [`tonic::Status`], allowing the implementation of the
//! [gRPC Richer Error Model] with [`tonic`] in a convenient way.
//!
//! [`tonic::Status`]: https://docs.rs/tonic/latest/tonic/struct.Status.html
//! [`tonic`]: https://docs.rs/tonic/latest/tonic/
//! [gRPC Richer Error Model]: https://www.grpc.io/docs/guides/error/

#![warn(
    missing_debug_implementations,
    missing_docs,
    rust_2018_idioms,
    unreachable_pub
)]
#![doc(
    html_logo_url = "https://raw.githubusercontent.com/tokio-rs/website/master/public/img/icons/tonic.svg"
)]
#![deny(rustdoc::broken_intra_doc_links)]
#![doc(html_root_url = "https://docs.rs/tonic-types/0.6.1")]
#![doc(issue_tracker_base_url = "https://github.com/hyperium/tonic/issues/")]

/// Useful protobuf types
pub mod pb {
    include!("generated/google.rpc.rs");
}

pub use pb::Status;

mod richer_error;

pub use richer_error::{
    BadRequest, ErrorDetail, ErrorDetails, FieldViolation, RetryInfo, StatusExt,
};

mod sealed {
    #[allow(unreachable_pub)]
    pub trait Sealed {}
}
