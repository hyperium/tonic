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

use std::path::Path;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // docs.rs won't let us download sources, so skip the C++ compile.
    if std::env::var("DOCS_RS").is_ok() {
        return;
    }

    // If CI/prebuilt environment tells us to skip the C++ build, do so immediately.
    if let Ok(val) = std::env::var("PROTOC_GEN_GRPC_RUST_NO_BUILD")
        && !val.is_empty()
        && val != "0"
    {
        // If the env var becomes unset, make sure we execute our build.rs again.
        println!("cargo:rerun-if-env-changed=PROTOC_GEN_GRPC_RUST_NO_BUILD");
        println!(
            "cargo:warning=PROTOC_GEN_GRPC_RUST_NO_BUILD is set, skipping C++ protobuf plugin build."
        );
        return;
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR env var is defined"));
    let cpp_source = Path::new("src").join("cpp_source");

    // Avoid rebuilding if the C++ source files (and this file) didn't change.
    println!("cargo:rerun-if-changed={}", cpp_source.to_str().unwrap());

    let install_dir = out_dir.join("install");
    if install_dir.exists() {
        std::fs::remove_dir_all(&install_dir)
            .expect("All files in install/ directory should be deletable");
    }

    let mut cmake_config = cmake::Config::new(&cpp_source);
    cmake_config.define("BUILD_PROTOC", "ON");
    cmake_config.define("BUILD_PLUGIN", "ON");
    cmake_config.define("CMAKE_INSTALL_PREFIX", &install_dir);
    // Make sure to not use stale configuration from a previous devel build, lest we get a bunch of
    // linker errors when building at various points in the tree without cargo clean-ing between.
    cmake_config.always_configure(true);
    cmake_config.configure_arg("--fresh");
    cmake_config.build();
}
