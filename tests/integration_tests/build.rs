fn main() {
    tonic_build::compile_protos("proto/test.proto").unwrap();
    tonic_build::compile_protos("proto/stream.proto").unwrap();
}
