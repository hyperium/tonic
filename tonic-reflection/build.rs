use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let reflection_descriptor =
        PathBuf::from(env::var("OUT_DIR").unwrap()).join("reflection_v1alpha1.bin");

    tonic_build::configure()
        .file_descriptor_set_path(&reflection_descriptor)
        .build_server(true)
        .build_client(true) // Client is only used for tests
        .format(true)
        .compile(&["proto/reflection.proto"], &["proto/"])?;

    Ok(())
}
