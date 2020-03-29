fn main() {
    tonic_build::compile_protos("proto/includer.proto").unwrap();
}
