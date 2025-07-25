fn main() {
    let proto = "proto/grpc/testing/test.proto";

    tonic_build::compile_protos(proto).unwrap();
    tonic_protobuf_build::CodeGen::new()
        .include("proto/grpc/testing")
        .inputs(["test.proto", "empty.proto", "messages.proto"])
        .compile()
        .unwrap();

    // prevent needing to rebuild if files (or deps) haven't changed
    println!("cargo:rerun-if-changed={proto}");
}
