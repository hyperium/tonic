fn main() {
    tonic_prost_build::compile_protos("proto/foo.proto").unwrap();
}
