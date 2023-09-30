fn main() {
    tonic_build::configure()
        .local_executor(true)
        .build_server(true)
        .compile(&["proto/test.proto", "proto/stream.proto"], &["proto"])
        .unwrap();
}
