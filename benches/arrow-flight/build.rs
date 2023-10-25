fn main() {
    tonic_build::configure()
        .bytes(&["."])
        .generate_default_stubs(true)
        .compile(&["proto/flight.proto"], &["proto"])
        .unwrap();

    tonic_build::manual::Builder::new().compile(&[tonic_build::manual::Service::builder()
        .name("FlightService")
        .package("arrow.flight.protocol")
        .method(
            tonic_build::manual::Method::builder()
                .name("do_exchange")
                .route_name("DoExchange")
                .input_type("crate::arrow::FlightData")
                .output_type("crate::arrow::FlightData")
                .codec_path("crate::codec::FlightDataCodec")
                .client_streaming()
                .server_streaming()
                .build(),
        )
        .build()]);
}
