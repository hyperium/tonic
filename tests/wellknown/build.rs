fn main() {
    tonic_build::compile_protos("proto/wellknown.proto").unwrap();
}
