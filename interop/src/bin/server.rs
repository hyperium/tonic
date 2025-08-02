use interop::{server_prost, server_protobuf};
use std::str::FromStr;
use tonic::transport::Server;
use tonic::transport::{Identity, ServerTlsConfig};

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
            let test_service =
                server_protobuf::TestServiceServer::new(server_protobuf::TestService::default());
            let unimplemented_service = server_protobuf::UnimplementedServiceServer::new(
                server_protobuf::UnimplementedService::default(),
            );

            // Wrap this test_service with a service that will echo headers as trailers.
            let test_service_svc = server_protobuf::EchoHeadersSvc::new(test_service);

            builder
                .add_service(test_service_svc)
                .add_service(unimplemented_service)
                .serve(addr)
                .await?;
        }
    };

    Ok(())
}
