fn main() {
    tonic_build::configure()
        .use_arc_self(true)
        .compile(&["proto/test.proto"], &["proto"])
        .unwrap();
}
