fn main() {
    prost_build::compile_protos(&["proto/status.proto"], &["proto/"]).unwrap();
}
