fn main() {
    tonic_build::configure()
        .compile(&["proto/test.proto"], &["proto"])
        .unwrap();
}
