fn main() {
    let proto = "proto/grpc/testing/test.proto";

    eprintln!("{}", tonic_protobuf_build::protoc());
    let path = std::env::var("PATH").unwrap_or_default();
    unsafe { std::env::set_var("PATH", format!("{}:{}", path, tonic_protobuf_build::bin())); }

    tonic_prost_build::compile_protos(proto).unwrap();
    tonic_protobuf_build::CodeGen::new()
        .include("proto/grpc/testing")
        .inputs(["test.proto", "empty.proto", "messages.proto"])
        .compile()
        .unwrap();

    // prevent needing to rebuild if files (or deps) haven't changed
    println!("cargo:rerun-if-changed={proto}");
}
