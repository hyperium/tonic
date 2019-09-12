use structopt::StructOpt;
use tonic::Server;
use tonic_interop::server;

#[derive(StructOpt)]
struct Opts {
    #[structopt(long)]
    use_tls: bool,
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let matches = Opts::from_args();

    // let sub = tracing_fmt::FmtSubscriber::builder().finish();
    // tracing::subscriber::set_global_default(sub).unwrap();
    // let _ = tracing_log::LogTracer::init();
    pretty_env_logger::init();

    let addr = "127.0.0.1:10000".parse().unwrap();

    let test_service = server::create();

    let mut builder = Server::builder();

    if matches.use_tls {
        let ca = tokio::fs::read("tonic-interop/data/server1.pem").await?;
        let key = tokio::fs::read("tonic-interop/data/server1.key").await?;
        builder.tls(ca, key);
    }

    builder.serve(addr, test_service).await?;

    Ok(())
}
