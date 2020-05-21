fn main() {
    tonic_build::configure()
        .include_file_descriptor_set(true)
        .compile(&["proto/helloworld/helloworld.proto"], &["proto/"])
        .unwrap();
    tonic_build::compile_protos("proto/routeguide/route_guide.proto").unwrap();
    tonic_build::compile_protos("proto/echo/echo.proto").unwrap();
    tonic_build::compile_protos("proto/google/pubsub/pubsub.proto").unwrap();
}
