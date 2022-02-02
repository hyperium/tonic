use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let grpc_health_v1_descriptor_set_path: PathBuf =
        PathBuf::from(env::var("OUT_DIR").unwrap()).join("grpc_health_v1.bin");
    tonic_build::configure()
        .file_descriptor_set_path(grpc_health_v1_descriptor_set_path)
        .build_server(true)
        .build_client(true)
        .compile(&["proto/health.proto"], &["proto/"])?;

    Ok(())
}
