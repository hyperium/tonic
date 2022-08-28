use std::{path::PathBuf, process::Command};

#[test]
fn bootstrap() {
    let iface_files = &["proto/status.proto", "proto/error_details.proto"];
    let dirs = &["proto"];

    let out_dir = PathBuf::from(std::env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("generated");

    tonic_build::configure()
        .out_dir(format!("{}", out_dir.display()))
        .compile(iface_files, dirs)
        .unwrap();

    let status = Command::new("git")
        .arg("diff")
        .arg("--exit-code")
        .arg("--")
        .arg(format!("{}", out_dir.display()))
        .status()
        .unwrap();

    if !status.success() {
        panic!("You should commit the protobuf files");
    }
}
