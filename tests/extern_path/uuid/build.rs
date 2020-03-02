fn main() {
    prost_build::compile_protos(&["uuid/uuid.proto"], &["../proto/"]).unwrap();
}
