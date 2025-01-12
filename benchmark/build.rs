use std::{env, path::PathBuf};

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    let worker_service = out_dir.join("worker_service");
    let _ = std::fs::create_dir(worker_service.clone()); // This will panic below if the directory failed to create
    tonic_build::configure()
        .out_dir(worker_service)
        .compile_protos(
            &["proto/grpc/testing/worker_service.proto"],
            &["proto/grpc/testing/"],
        )
        .unwrap();

    let behchmark_service_path = out_dir.join("benchmark_service");
    let _ = std::fs::create_dir(behchmark_service_path.clone());

    let protobuf_service_dir = behchmark_service_path.join("protobuf");
    let _ = std::fs::create_dir(protobuf_service_dir.clone());
    tonic_build::configure()
        .out_dir(protobuf_service_dir)
        .compile_protos(
            &["proto/grpc/testing/benchmark_service.proto"],
            &["proto/grpc/testing/"],
        )
        .unwrap();
}
