fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .format(false)
        .compile(&["proto/health.proto"], &["proto/"])?;

    Ok(())
}
