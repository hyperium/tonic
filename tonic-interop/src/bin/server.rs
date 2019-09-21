use structopt::StructOpt;
use tonic::Server;
use tonic_interop::{server, MergeTrailers};
// TODO: move GrpcService out of client since it can be used for the
// server too.
use http::header::HeaderName;
use tonic::body::BoxBody;
use tonic::client::GrpcService;

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

    let test_service = server::create();

    let mut builder = Server::builder();

    if matches.use_tls {
        let ca = tokio::fs::read("tonic-interop/data/server1.pem").await?;
        let key = tokio::fs::read("tonic-interop/data/server1.key").await?;
        builder.tls(ca, key);
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

    builder.serve(addr, test_service).await?;

    Ok(())
}
