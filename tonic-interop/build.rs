fn main() {
    let files = &["proto/grpc/testing/test.proto"];
    let dirs = &["proto/grpc/testing"];

    tonic_build::configure()
        .compile(files, dirs, "grpc.testing")
        .unwrap();

    // prevent needing to rebuild if files (or deps) haven't changed
    for file in files {
        println!("cargo:rerun-if-changed={}", file);
    }
}
