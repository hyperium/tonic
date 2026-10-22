/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

//! A `tonic` based gRPC Server Reflection implementation.

#![doc(
    html_logo_url = "https://github.com/grpc/grpc-rust/raw/master/.github/assets/tonic-docs.png"
)]
#![doc(issue_tracker_base_url = "https://github.com/grpc/grpc-rust/issues/")]
#![doc(test(no_crate_inject, attr(deny(rust_2018_idioms))))]
#![cfg_attr(docsrs, feature(doc_cfg))]

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
            FILE_DESCRIPTOR_SET_V1 as FILE_DESCRIPTOR_SET, grpc_reflection_v1::*,
        };
    }

    /// Generated protobuf types from the `grpc.reflection.v1alpha` package.
    pub mod v1alpha {
        pub use crate::generated::{
            FILE_DESCRIPTOR_SET_V1ALPHA as FILE_DESCRIPTOR_SET, grpc_reflection_v1alpha::*,
        };
    }
}

/// Implementation of the server component of gRPC Server Reflection.
#[cfg(feature = "server")]
pub mod server;
