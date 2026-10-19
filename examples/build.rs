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

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Optionally use protoc-gen-rust-grpc's protoc for prost. protoc-gen-rust-grpc will skip its
    // build when PROTOC_GEN_RUST_GRPC_NO_BUILD=1 (used in gRPC's CI), so we check that the binary
    // exists.
    #[cfg(feature = "protoc-gen-rust-grpc")]
    if protoc_gen_rust_grpc::protoc().exists() {
        unsafe {
            env::set_var("PROTOC", protoc_gen_rust_grpc::protoc());
        }
    }

    tonic_prost_build::configure()
        .compile_protos(&["proto/routeguide/route_guide.proto"], &["proto"])
        .unwrap();

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    tonic_prost_build::configure()
        .file_descriptor_set_path(out_dir.join("helloworld_descriptor.bin"))
        .compile_protos(&["proto/helloworld/helloworld.proto"], &["proto"])
        .unwrap();

    tonic_prost_build::compile_protos("proto/echo/echo.proto").unwrap();

    tonic_prost_build::compile_protos("proto/unaryecho/echo.proto").unwrap();

    tonic_prost_build::configure()
        .server_mod_attribute("attrs", "#[cfg(feature = \"server\")]")
        .server_attribute("Echo", "#[derive(PartialEq)]")
        .client_mod_attribute("attrs", "#[cfg(feature = \"client\")]")
        .client_attribute("Echo", "#[derive(PartialEq)]")
        .compile_protos(&["proto/attrs/attrs.proto"], &["proto"])
        .unwrap();

    tonic_prost_build::configure()
        .build_server(false)
        .compile_protos(
            &["proto/googleapis/google/pubsub/v1/pubsub.proto"],
            &["proto/googleapis"],
        )
        .unwrap();

    build_json_codec_service();

    let smallbuff_copy = out_dir.join("smallbuf");
    let _ = std::fs::create_dir(smallbuff_copy.clone()); // This will panic below if the directory failed to create
    tonic_prost_build::configure()
        .out_dir(smallbuff_copy)
        .codec_path("crate::common::SmallBufferCodec")
        .compile_protos(&["proto/helloworld/helloworld.proto"], &["proto"])
        .unwrap();

    println!("cargo:rerun-if-env-changed=GRPC_RUST_REGENERATE_PROTO");
    let grpc_helloworld = env::var_os("CARGO_FEATURE_GRPC_HELLOWORLD").is_some();
    let grpc_routeguide = env::var_os("CARGO_FEATURE_GRPC_ROUTEGUIDE").is_some();

    if (grpc_helloworld || grpc_routeguide) && env::var_os("GRPC_RUST_REGENERATE_PROTO").is_some() {
        let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());

        let generated_dir = manifest_dir.join("generated");
        if generated_dir.exists() {
            std::fs::remove_dir_all(&generated_dir)
                .expect("All files in generated/ directory should be deletable");
        }

        grpc_protobuf_build::CodeGen::new()
            .output_dir(generated_dir.join("helloworld"))
            .input("helloworld.proto")
            .include(manifest_dir.join("proto/helloworld"))
            .client_only()
            .compile()
            .unwrap();

        grpc_protobuf_build::CodeGen::new()
            .output_dir(generated_dir.join("routeguide"))
            .input("route_guide.proto")
            .include(manifest_dir.join("proto/routeguide"))
            .compile()
            .unwrap();
    }

    if env::var_os("CARGO_FEATURE_GRPC_GCP").is_some() {
        let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
        let dependencies = protobuf_well_known_types::get_dependency("protobuf_well_known_types")
            .into_iter()
            .map(|d| d.into())
            .collect();

        grpc_protobuf_build::CodeGen::new()
            .include(manifest_dir.join("proto/googleapis"))
            .inputs([
                "google/pubsub/v1/pubsub.proto",
                "google/pubsub/v1/schema.proto",
                "google/api/annotations.proto",
                "google/api/resource.proto",
                "google/api/http.proto",
                "google/api/field_behavior.proto",
                "google/api/client.proto",
                "google/protobuf/descriptor.proto", // bundled with protoc.
            ])
            .dependencies(dependencies)
            .client_only()
            .compile()
            .unwrap();
    }
}

// Manually define the json.helloworld.Greeter service which used a custom JsonCodec to use json
// serialization instead of protobuf for sending messages on the wire.
// This will result in generated client and server code which relies on its request, response and
// codec types being defined in a module `crate::common`.
//
// See the client/server examples defined in `src/json-codec` for more information.
fn build_json_codec_service() {
    let greeter_service = tonic_prost_build::manual::Service::builder()
        .name("Greeter")
        .package("json.helloworld")
        .method(
            tonic_prost_build::manual::Method::builder()
                .name("say_hello")
                .route_name("SayHello")
                .input_type("crate::common::HelloRequest")
                .output_type("crate::common::HelloResponse")
                .codec_path("crate::common::JsonCodec")
                .build(),
        )
        .build();

    tonic_prost_build::manual::Builder::new().compile(&[greeter_service]);
}
