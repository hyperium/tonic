fn main() {
    tonic_build::compile_protos("proto/result.proto").unwrap();
}
