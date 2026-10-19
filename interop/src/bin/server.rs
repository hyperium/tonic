/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

use std::str::FromStr;

use interop::server_prost;
use interop::server_protobuf;
use tonic::transport::Identity;
use tonic::transport::Server;
use tonic::transport::ServerTlsConfig;

#[derive(Debug)]
struct Opts {
    use_tls: bool,
    codec: Codec,
}

#[derive(Debug)]
enum Codec {
    Prost,
    Protobuf,
}

impl FromStr for Codec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "prost" => Ok(Codec::Prost),
            "protobuf" => Ok(Codec::Protobuf),
            _ => Err(format!("Invalid codec: {}", s)),
        }
    }
}

impl Opts {
    fn parse() -> Result<Self, pico_args::Error> {
        let mut pargs = pico_args::Arguments::from_env();
        Ok(Self {
            use_tls: pargs.contains("--use_tls"),
            codec: pargs.value_from_str("--codec")?,
        })
    }
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    interop::trace_init();

    let matches = Opts::parse()?;

    let addr = "127.0.0.1:10000".parse().unwrap();

    let mut builder = Server::builder();

    if matches.use_tls {
        let cert = std::fs::read_to_string("interop/data/server1.pem")?;
        let key = std::fs::read_to_string("interop/data/server1.key")?;
        let identity = Identity::from_pem(cert, key);

        builder = builder.tls_config(ServerTlsConfig::new().identity(identity))?;
    }

    match matches.codec {
        Codec::Prost => {
            let test_service =
                server_prost::TestServiceServer::new(server_prost::TestService::default());
            let unimplemented_service = server_prost::UnimplementedServiceServer::new(
                server_prost::UnimplementedService::default(),
            );

            // Wrap this test_service with a service that will echo headers as trailers.
            let test_service_svc = server_prost::EchoHeadersSvc::new(test_service);

            builder
                .add_service(test_service_svc)
                .add_service(unimplemented_service)
                .serve(addr)
                .await?;
        }
        Codec::Protobuf => {
            let test_service = server_protobuf::TestServiceServer::new(
                server_protobuf::InteropTestService::default(),
            );
            let unimplemented_service = server_protobuf::UnimplementedServiceServer::new(
                server_protobuf::UnimplementedInteropService::default(),
            );

            let _server = grpc::server::Server::builder()
                .interceptor(server_protobuf::EchoHeaders::new())
                .add_service(test_service)
                .add_service(unimplemented_service)
                .build();

            unimplemented!("gRPC server transport is not implemented yet");
        }
    };

    Ok(())
}
