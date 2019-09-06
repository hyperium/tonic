fn main() {
    tonic_build::compile_protos(
        &["proto/helloworld/helloworld.proto"],
        &["proto/helloworld"],
        "helloworld",
    )
    .unwrap();

    tonic_build::compile_protos(
        &["proto/routeguide/route_guide.proto"],
        &["proto/routeguide"],
        "routeguide",
    )
    .unwrap();
}
