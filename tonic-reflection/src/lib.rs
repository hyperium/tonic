//! A `tonic` based gRPC Server Reflection implementation.

#![doc(
    html_logo_url = "https://github.com/hyperium/tonic/raw/master/.github/assets/tonic-docs.png"
)]
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

    #[rustfmt::skip]
    pub mod reflection_v1_fds;

    #[rustfmt::skip]
    pub mod reflection_v1alpha1_fds;

    pub use reflection_v1_fds::FILE_DESCRIPTOR_SET as FILE_DESCRIPTOR_SET_V1;
    pub use reflection_v1alpha1_fds::FILE_DESCRIPTOR_SET as FILE_DESCRIPTOR_SET_V1ALPHA;

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
