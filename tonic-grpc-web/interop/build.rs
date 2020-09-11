fn main() {
    let protos = &["proto/test.proto"];

    tonic_build::configure()
        .build_client(false)
        .compile(protos, &["proto"])
        .unwrap();

    protos
        .iter()
        .for_each(|file| println!("cargo:rerun-if-changed={}", file));
}
