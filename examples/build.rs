use std::{env, path::PathBuf};

fn main() {
    build_grpc();

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

fn build_grpc() {
    let proto = "proto/routeguide/route_guide.proto";

    eprintln!("{}", grpc_protobuf_build::protoc());
    let path = std::env::var("PATH").unwrap_or_default();
    unsafe {
        std::env::set_var("PATH", format!("{}:{}", path, grpc_protobuf_build::bin()));
    }

    grpc_protobuf_build::CodeGen::new()
        .include("proto/routeguide")
        .inputs(["route_guide.proto"])
        .output_dir("src/grpc-routeguide/generated")
        .client_only()
        .compile()
        .unwrap();

    // prevent needing to rebuild if files (or deps) haven't changed
    println!("cargo:rerun-if-changed={proto}");
}
