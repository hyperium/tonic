use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let reflection_descriptors =
        PathBuf::from(env::var("OUT_DIR").unwrap()).join("reflection_descriptor.bin");

    tonic_build::configure()
        .file_descriptor_set_path(&reflection_descriptors)
        .build_server(true)
        .build_client(false)
        .format(true)
        .compile(&["proto/reflection.proto"], &["proto/"])?;

    Ok(())
}
