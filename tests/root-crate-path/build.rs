fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .extern_path(".foo.bar.baz.Animal", "crate::Animal")
        .compile_protos(&["foo.proto"], &["."])?;

    Ok(())
}
