fn main() -> Result<(), std::io::Error> {
    tonic_build::configure()
        .build_server(false)
        .build_client(true)
        .extern_path(".uuid", "::uuid")
        .compile_protos(
            &["service.proto", "uuid.proto"],
            &["../proto/my_application", "../proto/uuid"],
        )?;
    Ok(())
}
