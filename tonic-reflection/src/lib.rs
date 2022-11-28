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
#![doc(html_root_url = "https://docs.rs/tonic-reflection/0.6.0")]
#![doc(issue_tracker_base_url = "https://github.com/hyperium/tonic/issues/")]
#![doc(test(no_crate_inject, attr(deny(rust_2018_idioms))))]
#![cfg_attr(docsrs, feature(doc_cfg))]

/// Generated protobuf types from the `grpc.reflection.v1alpha` package.
pub mod proto {
    #![allow(unreachable_pub)]
    #![allow(missing_docs)]
    include!("generated/grpc.reflection.v1alpha.rs");

    pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!("generated/reflection_v1alpha1.bin");
}

/// Implementation of the server component of gRPC Server Reflection.
pub mod server;
