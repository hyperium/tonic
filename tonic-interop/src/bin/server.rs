use http::header::HeaderName;
use structopt::StructOpt;
use tonic::body::BoxBody;
use tonic::client::GrpcService;
use tonic::transport::{Identity, Server, ServerTlsConfig};
use tonic_interop::{server, MergeTrailers};

#[derive(StructOpt)]
struct Opts {
    #[structopt(long)]
    use_tls: bool,
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    tonic_interop::trace_init();

    let matches = Opts::from_args();

    let addr = "127.0.0.1:10000".parse().unwrap();

    let mut builder = Server::builder();

    if matches.use_tls {
        let cert = tokio::fs::read("tonic-interop/data/server1.pem").await?;
        let key = tokio::fs::read("tonic-interop/data/server1.key").await?;

        let identity = Identity::from_pem(cert, key);
        builder.tls_config(ServerTlsConfig::with_openssl().identity(identity));
    }

    builder.interceptor_fn(|svc, req| {
        let echo_header = req
            .headers()
            .get("x-grpc-test-echo-initial")
            .map(Clone::clone);

        let echo_trailer = req
            .headers()
            .get("x-grpc-test-echo-trailing-bin")
            .map(Clone::clone)
            .map(|v| (HeaderName::from_static("x-grpc-test-echo-trailing-bin"), v));

        let call = svc.call(req);

        async move {
            let mut res = call.await?;

            if let Some(echo_header) = echo_header {
                res.headers_mut()
                    .insert("x-grpc-test-echo-initial", echo_header);
            }

            Ok(res
                .map(|b| MergeTrailers::new(b, echo_trailer))
                .map(BoxBody::new))
        }
    });

    let test_service = server::TestServiceServer::new(server::TestService::default());
    let unimplemented_service =
        server::UnimplementedServiceServer::new(server::UnimplementedService::default());

    builder
        .add_service(test_service)
        .add_service(unimplemented_service)
        .serve(addr)
        .await?;

    Ok(())
}
