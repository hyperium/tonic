fn main() {
    cmake::build("../protoc-gen-rust-grpc");

    println!("cargo:rerun-if-changed=../protoc-gen-rust-grpc/cmake");
    println!("cargo:rerun-if-changed=../protoc-gen-rust-grpc/CMakeLists.txt");
}
