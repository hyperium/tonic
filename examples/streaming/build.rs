fn main() {
    tonic_build::configure()
        .compile(&["../proto/echo/echo.proto"], &["../proto"])
        .unwrap();
}
