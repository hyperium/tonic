fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .include_file_descriptor_set(true)
        .build_server(true)
        .build_client(false)
        .format(true)
        .compile(&["proto/reflection.proto"], &["proto/"])?;

    Ok(())
}
