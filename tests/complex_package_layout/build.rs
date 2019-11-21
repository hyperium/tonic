fn main() {
    tonic_build::configure()
        .compile(&["proto/hello.proto", "proto/goodbye.proto"], &["proto"])
        .unwrap();

    println!("cargo:rerun-if-changed=proto/hello.proto");
    println!("cargo:rerun-if-changed=proto/goodbye.proto");
    println!("cargo:rerun-if-changed=proto/types.proto");
}
