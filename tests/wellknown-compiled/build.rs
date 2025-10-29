fn main() {
    tonic_prost_build::configure()
        .extern_path(".google.protobuf.Empty", "()")
        .compile_well_known_types(true)
        .compile_protos(&["proto/google.proto", "proto/test.proto"], &["proto"])
        .unwrap();
}
