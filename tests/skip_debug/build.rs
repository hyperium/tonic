fn main() {
    let config = prost_build::Config::default();
    tonic_build::configure()
        .skip_debug("test.Test")
        .skip_debug("test.Output")
        .build_client(true)
        .build_server(true)
        .compile_protos_with_config(config, &["proto/test.proto"], &["proto"])
        .unwrap();
}
