#![cfg(unix)]
use std::error::Error;

use benchmark::worker::{
    self, payload_config::Payload::SimpleParams, server_args,
    worker_service_client::WorkerServiceClient, CoreRequest, PayloadConfig, SecurityParams,
    ServerArgs, ServerConfig, SimpleProtoParams, Void,
};
use clap::Parser;
use tonic::{transport::Channel, Request};

#[derive(Parser, Debug)]
struct Args {
    /// Port on which grpc.testing.WorkerService is running.
    #[arg(long = "worker_port")]
    worker_port: u16,
}

async fn run_server_stream(
    client: &mut WorkerServiceClient<Channel>,
) -> Result<(), Box<dyn Error>> {
    let server_config = ServerConfig {
        security_params: Some(SecurityParams {
            use_test_ca: true,
            ..Default::default()
        }),
        payload_config: Some(PayloadConfig {
            payload: Some(SimpleParams(SimpleProtoParams::default())),
        }),
        ..Default::default()
    };

    // The sequence of events is as follows:
    // 1. Send a request with server configuration to start the server.
    // 2. Receive the server status.
    // 3. Get server stats.
    // 4. End the stream.
    let outbound = async_stream::stream! {
        yield ServerArgs{
           argtype: Some(server_args::Argtype::Setup(server_config)),
        };
        yield ServerArgs {
            argtype: Some(server_args::Argtype::Mark(worker::Mark {
                reset: false,
            }))
        };
    };

    let response = client.run_server(Request::new(outbound)).await?;
    let mut inbound = response.into_inner();

    while let Some(note) = inbound.message().await? {
        println!("Server Stream response = {:?}", note);
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    println!(
        "Testing worker service running on port {}",
        args.worker_port
    );

    let endpoint = Channel::from_shared(format!("http://localhost:{}", args.worker_port))?;
    let mut client = WorkerServiceClient::connect(endpoint).await?;

    println!("Getting core count.");
    let core_count = client
        .core_count(CoreRequest::default())
        .await?
        .into_inner();
    assert!(core_count.cores > 0);

    println!("Running server stream.");
    run_server_stream(&mut client).await?;

    println!("Shutting down worker.");
    client.quit_worker(Void::default()).await?;
    Ok(())
}

#[cfg(not(unix))]
fn main() {}
