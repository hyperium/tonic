use interop::{server_prost, server_protobuf};
use std::str::FromStr;
use tonic::transport::Server;
use tonic::transport::{Identity, ServerTlsConfig};

#[derive(Debug)]
struct Opts {
    use_tls: bool,
    codec: Codec,
    port: u16,
    address_type: AddressType,
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

#[derive(Debug, Clone, Copy)]
enum AddressType {
    Ipv4,
    Ipv6,
    Ipv4Ipv6,
}

impl FromStr for AddressType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "IPV4" => Ok(AddressType::Ipv4),
            "IPV6" => Ok(AddressType::Ipv6),
            "IPV4_IPV6" => Ok(AddressType::Ipv4Ipv6),
            _ => Err(format!("Invalid address type: {}", s)),
        }
    }
}

impl Opts {
    fn parse() -> Result<Self, pico_args::Error> {
        let mut pargs = pico_args::Arguments::from_env();
        Ok(Self {
            use_tls: pargs.contains("--use_tls"),
            codec: pargs.value_from_str("--codec")?,
            port: pargs.opt_value_from_str("--port")?.unwrap_or(10000),
            address_type: pargs
                .opt_value_from_str("--address_type")?
                .unwrap_or(AddressType::Ipv4Ipv6),
        })
    }
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    interop::trace_init();

    let matches = Opts::parse()?;

    let host = match matches.address_type {
        AddressType::Ipv4 => "127.0.0.1",
        AddressType::Ipv6 => "[::1]",
        AddressType::Ipv4Ipv6 => "[::]",
    };
    let addr = format!("{host}:{}", matches.port).parse().unwrap();

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
                server_prost::TestServiceServer::new(server_prost::TestService::default())
                    .accept_compressed(tonic::codec::CompressionEncoding::Gzip)
                    .send_compressed(tonic::codec::CompressionEncoding::Gzip);
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
