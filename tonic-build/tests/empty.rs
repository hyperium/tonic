#[test]
fn empty() {
    let tmp = std::env::temp_dir();
    tonic_build::configure()
        .out_dir(tmp)
        .format(false)
        .compile(&["tests/protos/empty.proto"], &["tests/protos"])
        .unwrap();
}
