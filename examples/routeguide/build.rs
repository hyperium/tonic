fn main() {
    tonic_build::configure()
        .type_attribute("routeguide.Point", "#[derive(Hash)]")
        .compile(&["proto/routeguide.proto"], &["proto"])
        .unwrap();
}
