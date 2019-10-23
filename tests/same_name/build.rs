fn main() {
    tonic_build::compile_protos("proto/foo.proto").unwrap();
}
