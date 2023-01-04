use std::{path::PathBuf, process::Command};

#[test]
fn bootstrap() {
    let iface_files = &["proto/reflection.proto"];
    let dirs = &["proto"];

    let out_dir = PathBuf::from(std::env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("generated");

    tonic_build::configure()
        .build_client(true)
        .build_server(true)
        .build_transport(false)
        .file_descriptor_set_path(out_dir.join("reflection_v1alpha1.bin"))
        .out_dir(&out_dir)
        .compile(iface_files, dirs)
        .unwrap();

    let status = Command::new("git")
        .arg("diff")
        .arg("--exit-code")
        .arg("--")
        .arg(&out_dir)
        .status()
        .unwrap();

    assert!(status.success(), "You should commit the protobuf files");
}
