fn main() {
    prost_build::Config::new()
        .type_attribute(".", "#[allow(clippy::derive_partial_eq_without_eq)]")
        .compile_protos(&["uuid/uuid.proto"], &["../proto/"])
        .unwrap();
}
