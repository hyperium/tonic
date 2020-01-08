fn main() {
    tonic_build::prost::compile_protos("proto/includer.proto").unwrap();
}
