fn main() {
    tonic_build::prost::compile_protos("proto/wellknown.proto").unwrap();
}
