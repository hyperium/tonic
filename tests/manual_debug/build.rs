fn main() {
    tonic_build::configure()
        .build_client(false)
        .build_server(false)
        .skip_debug("ManualDebug")
        .compile(&["proto/test.proto"], &["proto"])
        .unwrap();
}
