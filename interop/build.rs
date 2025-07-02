fn main() {
    let proto = "proto/grpc/testing/test.proto";

    tonic_build::compile_protos(proto).unwrap();
    grpc_build::CodeGen::new()
        .include("proto/grpc/testing")
        .inputs(["test.proto", "empty.proto", "messages.proto"])
        .generate_and_compile()
        .unwrap();

    // prevent needing to rebuild if files (or deps) haven't changed
    println!("cargo:rerun-if-changed={proto}");
}
