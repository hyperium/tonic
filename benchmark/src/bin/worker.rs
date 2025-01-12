#![recursion_limit = "1024"]

use std::{pin::Pin, time::Duration};

use benchmark::server::BenchmarkServer;
use benchmark::worker::{
    server_args, worker_service_server::WorkerService, worker_service_server::WorkerServiceServer,
    ClientArgs, ClientStatus, CoreRequest, CoreResponse, ServerArgs, ServerStatus, Void,
};
use clap::Parser;
use tokio::{sync::mpsc, time};
use tokio_stream::{Stream, StreamExt};
use tonic::{transport::Server, Response, Status};

#[derive(Parser, Debug)]
struct Args {
    /// Port to expose grpc.testing.WorkerService, Used by driver to initiate work.
    #[arg(long = "driver_port")]
    driver_port: u16,
}

#[derive(Debug)]
struct DriverService {
    shutdown_channel: mpsc::Sender<()>,
}

#[tonic::async_trait]
impl WorkerService for DriverService {
    // Server streaming response type for the RunServer method.
    type RunServerStream =
        Pin<Box<dyn Stream<Item = Result<ServerStatus, Status>> + Send + 'static>>;

    async fn run_server(
        &self,
        request: tonic::Request<tonic::Streaming<ServerArgs>>,
    ) -> std::result::Result<Response<Self::RunServerStream>, Status> {
        println!("Handling server stream.");
        let mut stream = request.into_inner();

        let output = async_stream::try_stream! {
            let mut benchmark_server: Option<BenchmarkServer> = None;
            while let Some(request) = stream.next().await {
                let request = request?;
                let mut reset_stats = false;
                let argtype = request.argtype
                    .ok_or(Status::invalid_argument("missing request.argtype"))?;
                match  argtype {
                    server_args::Argtype::Setup(server_config) => {
                        println!("Server creation requested.");
                        if let Some(mut server) = benchmark_server.take() {
                            println!("server setup received when server already exists, shutting down the existing server");
                        }
                        match BenchmarkServer::start(server_config) {
                            Ok(server) => {
                                benchmark_server = Some(server);
                            },
                            Err(status) => {
                                println!("Error while creating server: {:?}", status);
                                Err(status)?;
                            }
                        }
                    },
                    server_args::Argtype::Mark(mark) => {
                        println!("Server stats requested.");
                        benchmark_server.as_ref()
                            .ok_or(Status::invalid_argument("server does not exist when mark received"))?;
                        reset_stats = mark.reset;
                    }
                };
                let server = benchmark_server.as_mut().unwrap();
                let stats = server.get_stats(reset_stats)?;
                yield ServerStatus {
                    stats: Some(stats),
                    cores: num_cpus::get() as i32,
                    port: server.port as i32,
                };
            }
        };

        Ok(Response::new(Box::pin(output) as Self::RunServerStream))
    }

    type RunClientStream =
        Pin<Box<dyn Stream<Item = Result<ClientStatus, Status>> + Send + 'static>>;

    async fn run_client(
        &self,
        _request: tonic::Request<tonic::Streaming<ClientArgs>>,
    ) -> std::result::Result<Response<Self::RunClientStream>, Status> {
        println!("Handling client stream.");
        todo!()
    }

    async fn core_count(
        &self,
        _request: tonic::Request<CoreRequest>,
    ) -> std::result::Result<Response<CoreResponse>, Status> {
        return Ok(Response::new(CoreResponse {
            cores: num_cpus::get() as i32,
        }));
    }

    async fn quit_worker(
        &self,
        _request: tonic::Request<Void>,
    ) -> std::result::Result<Response<Void>, Status> {
        match self.shutdown_channel.send(()).await {
            Ok(()) => Ok(Response::new(Void {})),
            Err(err) => Err(Status::internal(format!("failed to stop worker: {}", err))),
        }
    }
}

async fn run_worker() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    println!("{:?}", args);

    let addr = format!("0.0.0.0:{}", args.driver_port).parse().unwrap();
    let (tx, mut rx) = mpsc::channel(1);

    let svc = WorkerServiceServer::new(DriverService {
        shutdown_channel: tx,
    });

    Server::builder()
        .add_service(svc)
        .serve_with_shutdown(addr, async {
            rx.recv().await;
            // Wait for the quit_worker response to be sent.
            time::sleep(Duration::from_secs(1)).await;
        })
        .await?;

    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // TODO: Figure out a way to set thread count based on client/server
    // configs, possibly by using separate runtimes for the worker and
    // client/server.
    // Tests run on k8s use specific machine sizes and don't depend on the
    // clients/servers to restrict their resource usage.
    let core_count = num_cpus::get();
    println!("Creating a runtime with {} threads", core_count);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .thread_name("worker-pool")
        .worker_threads(core_count)
        .enable_all()
        .build()?;

    runtime.block_on(run_worker())?;
    Ok(())
}
