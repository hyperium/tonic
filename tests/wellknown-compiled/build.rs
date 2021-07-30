fn main() {
    let mut config = prost_build::Config::new();
    config.extern_path(".google.protobuf.Empty", "()");

    tonic_build::configure()
        .compile_well_known_types(true)
        .compile_with_config(
            config,
            &["proto/google.proto", "proto/test.proto"],
            &["proto"],
        )
        .unwrap();
}
