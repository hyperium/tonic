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

/// Include generated proto server and client items.
///
/// You must specify the path of the proto file within the proto directory,
/// without the ".proto" extension.
///
/// ```rust,ignore
/// mod pb {
///     grpc::include_proto!("protos", "helloworld");
/// }
/// ```
///
/// # Note:
/// **This only works if the grpc-build output directory and the message path
/// is unmodified**.
/// The default output directory is set to the [`OUT_DIR`] environment variable
/// and the message path is set to `self`.
/// If the output directory has been modified, the following pattern may be used
/// instead of this macro.
///
/// If the message path is `self`.
/// ```rust,ignore
/// mod protos {
///     // Include message code.
///     include!("/relative/protobuf/directory/protos/generated.rs");
///     /// Include service code.
///     include!("/relative/protobuf/directory/proto/helloworld_grpc.pb.rs");
/// }
///```
///
/// If the message code is not in the same module. The following example uses
/// message path as `super::protos`.
/// ```rust,ignore
/// mod protos {
///     // Include message code.
///     include!("/relative/protobuf/directory/protos/generated.rs");
/// }
///
/// mod grpc {
///     /// Include service code.
///     include!("/relative/protobuf/directory/proto/helloworld_grpc.pb.rs");
/// }
/// ```
/// [`OUT_DIR`]: https://doc.rust-lang.org/cargo/reference/environment-variables.html#environment-variables-cargo-sets-for-build-scripts
#[macro_export]
macro_rules! include_proto {
    ($parent_dir:literal, $proto_file:literal) => {
        include!(concat!(env!("OUT_DIR"), "/", $parent_dir, "/generated.rs"));
        include!(concat!(
            env!("OUT_DIR"),
            "/",
            $parent_dir,
            "/",
            $proto_file,
            "_grpc.pb.rs"
        ));
    };
}
