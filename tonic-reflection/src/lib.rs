//! A `tonic` based gRPC Server Reflection implementation.

#![warn(
    missing_debug_implementations,
    missing_docs,
    rust_2018_idioms,
    unreachable_pub
)]
#![doc(
    html_logo_url = "https://github.com/hyperium/tonic/raw/master/.github/assets/tonic-docs.png"
)]
#![deny(rustdoc::broken_intra_doc_links)]
#![doc(html_root_url = "https://docs.rs/tonic-reflection/0.12.1")]
#![doc(issue_tracker_base_url = "https://github.com/hyperium/tonic/issues/")]
#![doc(test(no_crate_inject, attr(deny(rust_2018_idioms))))]
#![cfg_attr(docsrs, feature(doc_auto_cfg))]

mod generated {
    #![allow(unreachable_pub)]
    #![allow(missing_docs)]
    #![allow(rustdoc::invalid_html_tags)]

    #[rustfmt::skip]
    pub mod grpc_reflection_v1alpha;

    #[rustfmt::skip]
    pub mod grpc_reflection_v1;

    /// Byte encoded FILE_DESCRIPTOR_SET.
    pub const FILE_DESCRIPTOR_SET_V1ALPHA: &[u8] =
        include_bytes!("generated/reflection_v1alpha1.bin");

    /// Byte encoded FILE_DESCRIPTOR_SET.
    pub const FILE_DESCRIPTOR_SET_V1: &[u8] = include_bytes!("generated/reflection_v1.bin");

    #[cfg(test)]
    mod tests {
        use super::{FILE_DESCRIPTOR_SET_V1, FILE_DESCRIPTOR_SET_V1ALPHA};
        use prost::Message as _;

        #[test]
        fn v1alpha_file_descriptor_set_is_valid() {
            prost_types::FileDescriptorSet::decode(FILE_DESCRIPTOR_SET_V1ALPHA).unwrap();
        }

        #[test]
        fn v1_file_descriptor_set_is_valid() {
            prost_types::FileDescriptorSet::decode(FILE_DESCRIPTOR_SET_V1).unwrap();
        }
    }
}

/// Generated protobuf types from the `grpc.reflection` namespace.
pub mod pb {
    /// Generated protobuf types from the `grpc.reflection.v1` package.
    pub mod v1 {
        pub use crate::generated::{
            grpc_reflection_v1::*, FILE_DESCRIPTOR_SET_V1 as FILE_DESCRIPTOR_SET,
        };
    }

    /// Generated protobuf types from the `grpc.reflection.v1alpha` package.
    pub mod v1alpha {
        pub use crate::generated::{
            grpc_reflection_v1alpha::*, FILE_DESCRIPTOR_SET_V1ALPHA as FILE_DESCRIPTOR_SET,
        };
    }
}

/// Implementation of the server component of gRPC Server Reflection.
#[cfg(feature = "server")]
pub mod server;
