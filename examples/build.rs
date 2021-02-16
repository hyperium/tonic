use std::env;
use std::path::PathBuf;

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

    tonic_build::configure()
        .build_server(false)
        .compile(
            &["proto/googleapis/google/pubsub/v1/pubsub.proto"],
            &["proto/googleapis"],
        )
        .unwrap();
}
