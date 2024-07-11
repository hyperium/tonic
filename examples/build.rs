use std::{env, path::PathBuf};

fn main() {
    tonic_build::configure()
        .type_attribute("routeguide.Point", "#[derive(Hash)]")
        .compile(&["proto/routeguide/route_guide.proto"], &["proto"])
        .unwrap();

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    tonic_build::configure()
        .file_descriptor_set_path(out_dir.join("helloworld_descriptor.bin"))
        .compile(&["proto/helloworld/helloworld.proto"], &["proto"])
        .unwrap();

    tonic_build::compile_protos("proto/echo/echo.proto").unwrap();

    tonic_build::compile_protos("proto/unaryecho/echo.proto").unwrap();

    tonic_build::configure()
        .server_mod_attribute("attrs", "#[cfg(feature = \"server\")]")
        .server_attribute("Echo", "#[derive(PartialEq)]")
        .client_mod_attribute("attrs", "#[cfg(feature = \"client\")]")
        .client_attribute("Echo", "#[derive(PartialEq)]")
        .compile(&["proto/attrs/attrs.proto"], &["proto"])
        .unwrap();

    tonic_build::configure()
        .build_server(false)
        .compile(
            &["proto/googleapis/google/pubsub/v1/pubsub.proto"],
            &["proto/googleapis"],
        )
        .unwrap();

    build_json_codec_service();

    let smallbuff_copy = out_dir.join("smallbuf");
    let _ = std::fs::create_dir(smallbuff_copy.clone()); // This will panic below if the directory failed to create
    tonic_build::configure()
        .out_dir(smallbuff_copy)
        .codec_path("crate::common::SmallBufferCodec")
        .compile(&["proto/helloworld/helloworld.proto"], &["proto"])
        .unwrap();
}

// Manually define the json.helloworld.Greeter service which used a custom JsonCodec to use json
// serialization instead of protobuf for sending messages on the wire.
// This will result in generated client and server code which relies on its request, response and
// codec types being defined in a module `crate::common`.
//
// See the client/server examples defined in `src/json-codec` for more information.
fn build_json_codec_service() {
    let greeter_service = tonic_build::manual::Service::builder()
        .name("Greeter")
        .package("json.helloworld")
        .method(
            tonic_build::manual::Method::builder()
                .name("say_hello")
                .route_name("SayHello")
                .input_type("crate::common::HelloRequest")
                .output_type("crate::common::HelloResponse")
                .codec_path("crate::common::JsonCodec")
                .build(),
        )
        .build();

    tonic_build::manual::Builder::new().compile(&[greeter_service]);

    protobuf_codec::build();
}

mod protobuf_codec {
    use heck::ToSnakeCase;
    use protobuf::reflect::{FileDescriptor, MessageDescriptor};
    use protobuf_parse::Parser;
    use std::fs;
    use std::path::PathBuf;

    pub fn build() {
        protobuf_codegen::Codegen::new()
            .include("proto")
            .inputs(&["proto/helloworld/helloworld.proto"])
            .cargo_out_dir("protos")
            .run_from_script();

        let parser = Parser::new()
            .include("proto")
            .inputs(&["proto/helloworld/helloworld.proto"])
            .parse_and_typecheck()
            .unwrap();

        let file_descriptors =
            FileDescriptor::new_dynamic_fds(parser.file_descriptors, &[]).unwrap();

        build_service(&file_descriptors)
    }

    fn build_service(file_descriptors: &[FileDescriptor]) {
        let services = file_descriptors
            .iter()
            .flat_map(|file_descriptor| {
                file_descriptor
                    .services()
                    .map(move |service_descriptor| (file_descriptor, service_descriptor))
            })
            .map(|(file_descriptor, service_descriptor)| {
                let builder = tonic_build::manual::Service::builder()
                    .name(service_descriptor.proto().name())
                    .package(file_descriptor.package());
                let mut builder_container = Some(builder);

                for method in service_descriptor.methods() {
                    let method_descriptor = method.proto();

                    let output_type = method.output_type();
                    let input_type = method.input_type();

                    builder_container = builder_container.map(|builder| {
                        builder.method(
                            tonic_build::manual::Method::builder()
                                .name(method_descriptor.name().to_snake_case())
                                .route_name(method_descriptor.name())
                                .output_type(type_string(&output_type))
                                .input_type(type_string(&input_type))
                                .codec_path("crate::codec::ProtobufCodec")
                                .build(),
                        )
                    });
                }

                builder_container.unwrap().build()
            })
            .collect::<Vec<_>>();

        tonic_build::manual::Builder::new()
            .out_dir({
                let mut base = PathBuf::from(std::env::var("OUT_DIR").unwrap());
                base.push("protobuf_codec");
                fs::create_dir_all(&base).unwrap();
                base
            })
            .compile(&services);
    }

    fn type_string(message_descriptor: &MessageDescriptor) -> String {
        let path = message_descriptor
            .file_descriptor()
            .name()
            .split("/")
            .last()
            .unwrap()
            .strip_suffix(".proto")
            .unwrap();

        format!("crate::protos::{}::{}", path, message_descriptor.name())
    }
}
