fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(true)
        .build_client(false)
        .format(false)
        .compile(&["proto/health.proto"], &["proto/"])?;

    Ok(())
}
