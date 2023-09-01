fn main() {
    tonic_build::configure()
        .compile(&["proto/test.proto"], &["proto"])
        .unwrap();
    tonic_build::configure()
        .generate_default_stubs(true)
        .compile(&["proto/test_default.proto"], &["proto"])
        .unwrap();
}
