use std::{fs, path::PathBuf};

fn main() {
    let out_dir = PathBuf::from(std::env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("generated");
    fs::create_dir_all(out_dir.as_path()).unwrap();
    let mut config = prost_build::Config::default();
    config.disable_comments(&["test.Input1", "test.Output1"]);
    tonic_build::configure()
        .disable_comments("test.Service1")
        .disable_comments("test.Service1.Rpc1")
        .build_client(true)
        .build_server(true)
        .out_dir(format!("{}", out_dir.display()))
        .compile_with_config(config, &["proto/test.proto"], &["proto"])
        .unwrap();
}
