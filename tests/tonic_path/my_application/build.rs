fn main() -> Result<(), std::io::Error> {
    tonic_prost_build::configure()
        .tonic_path("wrapper::tonic")
        .codec_path("wrapper::tonic_prost::ProstCodec")
        .compile_protos(&["greeter/greeter.proto"], &["../proto"])?;
    Ok(())
}
