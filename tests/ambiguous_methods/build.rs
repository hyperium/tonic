fn main() {
    tonic_build::compile_protos("proto/ambiguous_methods.proto").unwrap();
}
