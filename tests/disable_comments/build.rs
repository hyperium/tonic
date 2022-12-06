fn main() {
    let mut config = prost_build::Config::default();
    config.disable_comments(["test.Input1", "test.Output1"]);
    tonic_build::configure()
        .disable_comments("test.Service1")
        .disable_comments("test.Service1.Rpc1")
        .build_client(true)
        .build_server(true)
        .compile_with_config(config, &["proto/test.proto"], &["proto"])
        .unwrap();
}
