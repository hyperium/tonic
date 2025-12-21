#[test]
fn assert_generated_code() {
    tonic_prost_build::configure()
        .async_trait(false)
        .build_server(true)
        .out_dir("tests")
        .compile_protos(&["tests/echo.proto"], &["tests"])
        .expect("Failed compiling!");

    let expected_without_rustfmt_skip =
        std::fs::read_to_string("tests/expected.grpc.examples.echo.rs")
            .expect("Failed reading expected")
            .lines()
            .skip(1)
            .collect::<Vec<_>>()
            .join("\n");

    let generated =
        std::fs::read_to_string("tests/grpc.examples.echo.rs").expect("Failed reading generated");

    assert_eq!(expected_without_rustfmt_skip.trim(), generated.trim());
    std::fs::remove_file("tests/grpc.examples.echo.rs").expect("Failed removing generated");
}
