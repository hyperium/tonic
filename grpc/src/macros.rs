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

/// Includes generated proto message, client, and server code.
///
/// You must specify the path to the `.proto` file
/// **relative to the proto root directory**,  without the `.proto` extension.  
///
/// For example, if your proto directory is `path/to/protos` and it contains the
/// file  `helloworld.proto`, you would write:
///
/// ```rust,ignore
/// mod pb {
///     grpc::include_proto!("path/to/protos", "helloworld");
/// }
/// ```
///
/// # Note
/// **This macro only works if the gRPC build output directory and message path
/// are unmodified.**
/// By default:
/// - The output directory is set to the [`OUT_DIR`] environment variable.
/// - The message path is set to `self`.
///
/// If you have modified the output directory or message path, you should
/// include the generated code manually instead of using this macro.
///
/// The following example assumes the message code is imported using `self`:
///
/// ```rust,ignore
/// mod protos {
///     // Include message code.
///     include!("/protobuf/directory/protos/generated.rs");
///
///     // Include service code.
///     include!("/protobuf/directory/protos/helloworld_grpc.pb.rs");
/// }
/// ```
///
/// If the message code and service code are in different modules, and the
/// message path specified during code generation is `super::protos`, use:
///
/// ```rust,ignore
/// mod protos {
///     // Include message code.
///     include!("/protobuf/directory/protos/generated.rs");
/// }
///
/// mod grpc {
///     // Include service code.
///     include!("/protobuf/directory/proto/helloworld_grpc.pb.rs");
/// }
/// ```
///
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
