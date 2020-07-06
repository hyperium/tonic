use interop::server;
use structopt::StructOpt;
use tonic::transport::Server;
use tonic::transport::{Identity, ServerTlsConfig};

#[derive(StructOpt)]
struct Opts {
    #[structopt(name = "use_tls", long)]
    use_tls: bool,
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    interop::trace_init();

    let matches = Opts::from_args();

    let addr = "127.0.0.1:10000".parse().unwrap();

    let mut builder = Server::builder();

    if matches.use_tls {
        let cert = tokio::fs::read("interop/data/server1.pem").await?;
        let key = tokio::fs::read("interop/data/server1.key").await?;
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
