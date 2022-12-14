fn main() {
    tonic_build::configure()
        .compile(&["../proto/helloworld/helloworld.proto"], &["../proto"])
        .unwrap();
}
