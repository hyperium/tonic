// This build file is used to generate the code as a one-off,
// but is only rerun with the `gen-proto` feature enabled.
// This simplifies the build process for this crate by not requiring
// users to have protoc available.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "gen-proto")]
    tonic_build::configure()
        .file_descriptor_set_path(
            std::path::PathBuf::from("src/generated").join("grpc_health_v1.bin"),
        )
        .out_dir("src/generated")
        .build_server(true)
        .build_client(true)
        .compile(&["proto/health.proto"], &["proto/"])?;

    Ok(())
}
