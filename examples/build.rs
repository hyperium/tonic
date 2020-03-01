fn main() {
    tonic_build::prost::compile_protos("proto/helloworld/helloworld.proto").unwrap();
    tonic_build::prost::compile_protos("proto/routeguide/route_guide.proto").unwrap();
    tonic_build::prost::compile_protos("proto/echo/echo.proto").unwrap();
    tonic_build::prost::compile_protos("proto/google/pubsub/pubsub.proto").unwrap();
}
