#[test]
fn wellknown() {
    let tmp = std::env::temp_dir();
    tonic_build::configure()
        .out_dir(tmp)
        .format(false)
        .type_attribute(".", "#[derive(Serialize, Deserialize)]")
        .type_attribute(".", "#[serde(rename_all = \"camelCase\")]")
        .field_attribute("in", "#[serde(rename = \"in\")]")
        .compile(&["tests/protos/wellknown.proto"], &["tests/protos"])
        .unwrap();
}
