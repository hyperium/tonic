fn main() {
    tonic_prost_build::compile_protos("proto/ambiguous_methods.proto").unwrap();
}
