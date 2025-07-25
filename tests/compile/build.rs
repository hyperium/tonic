fn main() {
    tonic_prost_build::compile_protos("proto/result.proto").unwrap();
    tonic_prost_build::compile_protos("proto/service.proto").unwrap();
    tonic_prost_build::compile_protos("proto/stream.proto").unwrap();
    tonic_prost_build::compile_protos("proto/same_name.proto").unwrap();
    tonic_prost_build::compile_protos("proto/ambiguous_methods.proto").unwrap();
    tonic_prost_build::compile_protos("proto/includer.proto").unwrap();
    tonic_prost_build::configure()
        .extern_path(".root_crate_path.Animal", "crate::Animal")
        .compile_protos(&["proto/root_crate_path.proto"], &["."])
        .unwrap();
    tonic_prost_build::configure()
        .skip_debug(["skip_debug.Test"])
        .skip_debug(["skip_debug.Output"])
        .build_client(true)
        .build_server(true)
        .compile_protos(&["proto/skip_debug.proto"], &["proto"])
        .unwrap();
    tonic_prost_build::configure()
        .use_arc_self(true)
        .compile_protos(&["proto/use_arc_self.proto"], &["proto"])
        .unwrap();
}
