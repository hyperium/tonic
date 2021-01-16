fn main() {
    tonic_build::configure()
        .type_attribute("routeguide.Point", "#[derive(Hash)]")
        .compile(&["proto/routeguide/route_guide.proto"], &["proto"])
        .unwrap();

    tonic_build::compile_protos("proto/helloworld/helloworld.proto").unwrap();
    tonic_build::compile_protos("proto/echo/echo.proto").unwrap();
    tonic_build::compile_protos("proto/google/pubsub/pubsub.proto").unwrap();
}
