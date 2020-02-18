fn main() -> Result<(), std::io::Error> {
    tonic_build::configure()
        .build_server(false)
        .build_client(true)
        .extern_path(".uuid", "::uuid")
        .compile(
            &["service.proto", "uuid.proto"],
            &["../proto/my_application", "../proto/uuid"],
        )?;
    Ok(())
}
