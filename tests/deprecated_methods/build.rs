fn main() {
    tonic_build::compile_protos("proto/test.proto").unwrap();
}
