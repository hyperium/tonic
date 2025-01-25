fn main() {
    tonic_build::configure()
        .compile_protos(&["proto/test.proto"], &["proto"])
        .unwrap();
    tonic_build::configure()
        .generate_default_stubs(true)
        .compile_protos(&["proto/test_default.proto"], &["proto"])
        .unwrap();
}
