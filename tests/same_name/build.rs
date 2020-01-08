fn main() {
    tonic_build::prost::compile_protos("proto/foo.proto").unwrap();
}
