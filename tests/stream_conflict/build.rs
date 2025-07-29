fn main() {
    tonic_prost_build::compile_protos("proto/stream_conflict.proto").unwrap();
}
