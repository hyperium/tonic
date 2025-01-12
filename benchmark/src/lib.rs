#![recursion_limit = "1024"]

pub mod server;

pub mod worker {
    include!(concat!(env!("OUT_DIR"), "/worker_service/grpc.testing.rs"));
}

pub mod protobuf_benchmark_service {
    include!(concat!(
        env!("OUT_DIR"),
        "/benchmark_service/protobuf/grpc.testing.rs"
    ));
}
