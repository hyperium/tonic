fn main() {
    let config = prost_build::Config::default();
    tonic_build::configure()
        .skip_debug("test.Test")
        .skip_debug("test.Output")
        .build_client(true)
        .build_server(true)
        .compile_with_config(config, &["proto/test.proto"], &["proto"])
        .unwrap();

    // Add a dummy impl Debug to the skipped debug implementations to avoid missing impl Debug errors
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let file_path = out.join("test.rs");
    let mut file_contents = std::fs::read_to_string(&file_path).unwrap();
    let debug_impl = r#"
impl std::fmt::Debug for Output {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Output").finish()
    }
}
"#;
    file_contents.push_str(debug_impl);

    // Replace the original file with the modified content
    std::fs::write(&file_path, file_contents).unwrap();
}
