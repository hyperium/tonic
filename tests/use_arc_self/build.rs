fn main() {
    tonic_prost_build::configure()
        .use_arc_self(true)
        .compile_protos(&["proto/test.proto"], &["proto"])
        .unwrap();
}
