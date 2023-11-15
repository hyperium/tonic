use std::path::{Path, PathBuf};

fn main() {
    // tonic-health
    codegen(
        &PathBuf::from(std::env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("tonic-health"),
        &["proto/health.proto"],
        &["proto"],
        &PathBuf::from("src/generated"),
        &PathBuf::from("src/generated/grpc_health_v1.bin"),
        true,
        true,
    );

    // tonic-reflection
    codegen(
        &PathBuf::from(std::env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("tonic-reflection"),
        &["proto/reflection.proto"],
        &["proto"],
        &PathBuf::from("src/generated"),
        &PathBuf::from("src/generated/reflection_v1alpha1.bin"),
        true,
        true,
    );

    // tonic-types
    codegen(
        &PathBuf::from(std::env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("tonic-types"),
        &["proto/status.proto", "proto/error_details.proto"],
        &["proto"],
        &PathBuf::from("src/generated"),
        &PathBuf::from("src/generated/types.bin"),
        false,
        false,
    );
}

fn codegen(
    root_dir: &Path,
    iface_files: &[&str],
    include_dirs: &[&str],
    out_dir: &Path,
    file_descriptor_set_path: &Path,
    build_client: bool,
    build_server: bool,
) {
    let tempdir = tempfile::Builder::new()
        .prefix("tonic-codegen-")
        .tempdir()
        .unwrap();

    let iface_files: Vec<PathBuf> = iface_files
        .iter()
        .map(|&path| root_dir.join(path))
        .collect();

    let include_dirs: Vec<PathBuf> = include_dirs
        .iter()
        .map(|&path| root_dir.join(path))
        .collect();
    let out_dir = root_dir.join(out_dir);
    let file_descriptor_set_path = root_dir.join(file_descriptor_set_path);

    tonic_build::configure()
        .build_client(build_client)
        .build_server(build_server)
        .out_dir(&tempdir)
        .file_descriptor_set_path(file_descriptor_set_path)
        .compile(&iface_files, &include_dirs)
        .unwrap();

    for path in std::fs::read_dir(tempdir.path()).unwrap() {
        let path = path.unwrap().path();
        let to = out_dir.join(
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .strip_suffix(".rs")
                .unwrap()
                .replace('.', "_")
                + ".rs",
        );
        std::fs::copy(&path, &to).unwrap();
    }
}
