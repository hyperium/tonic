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

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Use protoc-gen-grpc-rust for tonic_prost_build when it is available
    #[cfg(feature = "protoc-gen-grpc-rust")]
    if protoc_gen_grpc_rust::protoc().exists() {
        unsafe {
            std::env::set_var("PROTOC", protoc_gen_grpc_rust::protoc());
        }
    }

    tonic_prost_build::configure()
        .compile_protos(&["grpc/testing/test.proto"], &["proto"])
        .unwrap();
    grpc_protobuf_build::CodeGen::new()
        .include("proto")
        .inputs([
            "grpc/testing/test.proto",
            "grpc/testing/empty.proto",
            "grpc/testing/messages.proto",
        ])
        .compile()
        .unwrap();
}
