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

use std::env;
use std::path::PathBuf;

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let dependencies = protobuf_well_known_types::get_dependency("protobuf_well_known_types")
        .into_iter()
        .map(|d| d.into())
        .collect();

    grpc_protobuf_build::CodeGen::new()
        .include("proto")
        .inputs([
            "grpc/testing/benchmark_service.proto",
            "grpc/testing/messages.proto",
            "grpc/testing/worker_service.proto",
            "grpc/testing/control.proto",
            "grpc/testing/payloads.proto",
            "grpc/testing/stats.proto",
            "grpc/core/stats.proto",
        ])
        .dependencies(dependencies)
        .client_only()
        .compile()
        .unwrap();

    let services_tonic = out_dir.join("tonic");

    // TODO: Use gRPC servers when available.
    if protoc_gen_grpc_rust::protoc().exists() {
        unsafe {
            env::set_var("PROTOC", protoc_gen_grpc_rust::protoc());
        }
    }

    let _ = std::fs::create_dir(services_tonic.clone());
    tonic_prost_build::configure()
        .out_dir(services_tonic)
        .build_client(false)
        .compile_protos(
            &[
                "grpc/testing/benchmark_service.proto",
                "grpc/testing/worker_service.proto",
            ],
            &["proto"],
        )
        .unwrap();
}
