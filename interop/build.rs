fn main() {
    let proto = "proto/grpc/testing/test.proto";

    tonic_build::compile_protos(proto).unwrap();

    // prevent needing to rebuild if files (or deps) haven't changed
    println!("cargo:rerun-if-changed={}", proto);
}
