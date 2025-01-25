use interop::server;
use tonic::transport::Server;
use tonic::transport::{Identity, ServerTlsConfig};

#[derive(Debug)]
struct Opts {
    use_tls: bool,
}

impl Opts {
    fn parse() -> Result<Self, pico_args::Error> {
        let mut pargs = pico_args::Arguments::from_env();
        Ok(Self {
            use_tls: pargs.contains("--use_tls"),
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

    let test_service = server::TestServiceServer::new(server::TestService::default());
    let unimplemented_service =
        server::UnimplementedServiceServer::new(server::UnimplementedService::default());

    // Wrap this test_service with a service that will echo headers as trailers.
    let test_service_svc = server::EchoHeadersSvc::new(test_service);

    builder
        .add_service(test_service_svc)
        .add_service(unimplemented_service)
        .serve(addr)
        .await?;

    Ok(())
}
