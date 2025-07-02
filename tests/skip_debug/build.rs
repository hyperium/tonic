fn main() {
    tonic_prost_build::configure()
        .skip_debug(["test.Test"])
        .skip_debug(["test.Output"])
        .build_client(true)
        .build_server(true)
        .compile_protos(&["proto/test.proto"], &["proto"])
        .unwrap();
}
