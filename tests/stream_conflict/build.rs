fn main() {
    tonic_build::compile_protos("proto/stream_conflict.proto").unwrap();
}
