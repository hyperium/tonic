#![recursion_limit = "1024"]
#![cfg(unix)]

pub mod server;

#[allow(clippy::large_enum_variant)]
pub mod worker {
    include!(concat!(env!("OUT_DIR"), "/worker_service/grpc.testing.rs"));
}

pub mod protobuf_benchmark_service {
    include!(concat!(
        env!("OUT_DIR"),
        "/benchmark_service/protobuf/grpc.testing.rs"
    ));
}
