//! A `tonic` based gRPC healthcheck implementation.
//!
//! # Example
//!
//! An example can be found [here].
//!
//! [here]: https://github.com/hyperium/tonic/blob/master/examples/src/health/server.rs

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
#![doc(html_root_url = "https://docs.rs/tonic-health/0.10.2")]
#![doc(issue_tracker_base_url = "https://github.com/hyperium/tonic/issues/")]
#![doc(test(no_crate_inject, attr(deny(rust_2018_idioms))))]
#![cfg_attr(docsrs, feature(doc_cfg))]

use std::fmt::{Display, Formatter};

mod generated {
    #![allow(unreachable_pub)]
    #![allow(missing_docs)]
    #[rustfmt::skip]
    pub mod grpc_health_v1;

    /// Byte encoded FILE_DESCRIPTOR_SET.
    pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!("generated/grpc_health_v1.bin");

    #[cfg(test)]
    mod tests {
        use super::FILE_DESCRIPTOR_SET;
        use prost::Message as _;

        #[test]
        fn file_descriptor_set_is_valid() {
            prost_types::FileDescriptorSet::decode(FILE_DESCRIPTOR_SET).unwrap();
        }
    }
}

/// Generated protobuf types from the `grpc.health.v1` package.
pub mod pb {
    pub use crate::generated::{grpc_health_v1::*, FILE_DESCRIPTOR_SET};
}

pub mod server;

/// An enumeration of values representing gRPC service health.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ServingStatus {
    /// Unknown status
    Unknown,
    /// The service is currently up and serving requests.
    Serving,
    /// The service is currently down and not serving requests.
    NotServing,
}

impl Display for ServingStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            ServingStatus::Unknown => f.write_str("Unknown"),
            ServingStatus::Serving => f.write_str("Serving"),
            ServingStatus::NotServing => f.write_str("NotServing"),
        }
    }
}

impl From<ServingStatus> for pb::health_check_response::ServingStatus {
    fn from(s: ServingStatus) -> Self {
        match s {
            ServingStatus::Unknown => pb::health_check_response::ServingStatus::Unknown,
            ServingStatus::Serving => pb::health_check_response::ServingStatus::Serving,
            ServingStatus::NotServing => pb::health_check_response::ServingStatus::NotServing,
        }
    }
}
