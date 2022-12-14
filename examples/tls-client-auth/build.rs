fn main() {
    tonic_build::configure()
        .compile(&["../proto/unaryecho/echo.proto"], &["../proto"])
        .unwrap();
}
