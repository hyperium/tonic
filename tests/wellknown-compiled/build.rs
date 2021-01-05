fn main() {
    tonic_build::configure()
        .compile_well_known_types(true)
        .compile(&["proto/wellknown.proto"], &["proto"])
        .unwrap();
}
