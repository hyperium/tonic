#[test]
fn assert_generated_code() {
    tonic_prost_build::configure()
        .async_trait(false)
        .build_server(true)
        .out_dir("tests")
        .compile_protos(&["tests/echo.proto"], &["tests"])
        .expect("Failed compiling!");
    assert!(
        std::fs::read("tests/expected.grpc.examples.echo.rs").expect("Failed reading expected")
            == std::fs::read("tests/grpc.examples.echo.rs").expect("Failed reading generated")
    );
    std::fs::remove_file("tests/grpc.examples.echo.rs").expect("Failed removing generated");
}
