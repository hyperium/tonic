fn main() {
    tonic_build::configure()
        .compile(
            &[
                "../proto/helloworld/helloworld.proto",
                "../proto/unaryecho/echo.proto",
            ],
            &["../proto"],
        )
        .unwrap();
}
