fn main() {
    tonic_build::configure()
        .use_generic_streaming_requests(true)
        .compile_protos(&["proto/test.proto"], &["proto"])
        .unwrap();
}
