fn main() {
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    tonic_build::configure()
        .file_descriptor_set_path(out_dir.join("helloworld_descriptor.bin"))
        .compile(&["../proto/helloworld/helloworld.proto"], &["../proto"])
        .unwrap();
}
