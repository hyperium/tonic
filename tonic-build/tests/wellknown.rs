#[test]
fn wellknown() {
    let tmp = std::env::temp_dir();
    tonic_build::configure()
        .out_dir(tmp)
        .format(false)
        .compile(&["tests/protos/wellknown.proto"], &["tests/protos"])
        .unwrap();
}
