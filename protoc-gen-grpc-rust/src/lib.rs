/*
 *
 * Copyright 2026 gRPC authors.
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

//! Library for compiling and using the [`gRPC-Rust`] plugin for [`protoc`].
//!
//! [`protoc`]: https://protobuf.dev/installation/
//! [`gRPC-Rust`]: https://crates.io/crates/grpc

use std::path::PathBuf;

fn bin_file(file: &str) -> PathBuf {
    let mut path = bin().join(file);
    if cfg!(target_os = "windows") {
        path.set_extension("exe");
    }
    path
}

/// The full path to the `protoc` executable.
pub fn protoc() -> PathBuf {
    bin_file("protoc")
}

/// The full path to the gRPC `protoc` plugin, `protoc-gen-grpc-rust`.
pub fn protoc_gen_grpc_rust() -> PathBuf {
    bin_file("protoc-gen-grpc-rust")
}

/// The path to the `bin` directory containing the C++ binaries this package
/// builds.
pub fn bin() -> PathBuf {
    PathBuf::from(env!("OUT_DIR")).join("install/bin")
}
