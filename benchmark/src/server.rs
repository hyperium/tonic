use std::pin::Pin;

use nix::sys::{
    resource::{getrusage, Usage, UsageWho},
    time::TimeValLike,
};
use tokio_stream::{Stream, StreamExt};
use tokio_util::sync::CancellationToken;
use tonic::{
    transport::{Identity, Server, ServerTlsConfig},
    Request, Response, Status,
};

use crate::{
    protobuf_benchmark_service::{Payload, PayloadType, SimpleRequest, SimpleResponse},
    worker::{
        payload_config::Payload::{BytebufParams, ComplexParams, SimpleParams},
        ServerConfig, ServerStats, SimpleProtoParams,
    },
};

const DEFAULT_PORT: usize = 50055;

pub struct BenchmarkServer {
    last_reset_time: std::time::Instant,
    last_rusage: Usage,
    cancellation_token: CancellationToken,
    pub port: usize,
}

impl BenchmarkServer {
    pub fn start(config: ServerConfig) -> Result<Self, Status> {
        println!("{:?}", config);

        let mut server_builder = Server::builder();
        // Parse security config.
        if let Some(securit_params) = config.security_params {
            let tls_config = if securit_params.use_test_ca {
                let data_path = std::env::var("DATA_PATH")
                    .unwrap_or_else(|_| std::env!("CARGO_MANIFEST_DIR").to_string());
                let data_dir = std::path::PathBuf::from_iter([data_path, "data".to_string()]);
                println!("Loading TLS certs from {:?}", data_dir);
                let cert = std::fs::read_to_string(data_dir.join("tls/server.pem"))?;
                let key = std::fs::read_to_string(data_dir.join("tls/server.key"))?;
                ServerTlsConfig::new().identity(Identity::from_pem(cert, key))
            } else {
                ServerTlsConfig::new()
            };
            server_builder = server_builder.tls_config(tls_config).map_err(|err| {
                Status::invalid_argument(format!("failed to create TLS config: {}", err))
            })?;
        };

        // Parse payload config.
        let payload_type = match config.payload_config {
            Some(payload_config) => payload_config.payload.ok_or(Status::invalid_argument(
                "payload missing in payload_config",
            ))?,
            None => SimpleParams(SimpleProtoParams::default()),
        };

        let router = match payload_type {
            BytebufParams(_) | ComplexParams(_) => {
                return Err(Status::unimplemented("codec not implemented."))
            }
            SimpleParams(_) => {
                let server = crate::protobuf_benchmark_service::benchmark_service_server::BenchmarkServiceServer::new(ProtoServer{});
                server_builder.add_service(server)
            }
        };

        let cancellation_token = CancellationToken::new();
        let token_copy = cancellation_token.clone();
        let port = if config.port > 0 {
            config.port as usize
        } else {
            DEFAULT_PORT
        };
        let addr = format!("[::]:{}", port).parse().unwrap();
        tokio::spawn(router.serve_with_shutdown(addr, async move {
            token_copy.cancelled().await;
            println!("Server is shutting down.")
        }));

        Ok(BenchmarkServer {
            last_reset_time: std::time::Instant::now(),
            last_rusage: getrusage(UsageWho::RUSAGE_SELF).map_err(|err| {
                Status::internal(format!("failed to query system resource usage: {}", err))
            })?,
            cancellation_token,
            port,
        })
    }

    pub fn get_stats(&mut self, reset: bool) -> Result<ServerStats, Status> {
        let now = std::time::Instant::now();
        let wall_time_elapsed = now.duration_since(self.last_reset_time);
        let latest_rusage = getrusage(UsageWho::RUSAGE_SELF).map_err(|err| {
            Status::internal(format!("failed to query system resource usage: {}", err))
        })?;
        let user_time = latest_rusage.user_time() - self.last_rusage.user_time();
        let system_time = latest_rusage.system_time() - self.last_rusage.system_time();

        if reset {
            self.last_rusage = latest_rusage;
            self.last_reset_time = now;
        }

        Ok(ServerStats {
            time_elapsed: wall_time_elapsed.as_nanos() as f64 / 1e9,
            time_user: user_time.num_nanoseconds() as f64 / 1e9,
            time_system: system_time.num_nanoseconds() as f64 / 1e9,
            ..Default::default()
        })
    }
}

#[derive(Clone, Debug)]
struct ProtoServer {}

#[tonic::async_trait]
impl crate::protobuf_benchmark_service::benchmark_service_server::BenchmarkService for ProtoServer {
    async fn unary_call(
        &self,
        request: Request<SimpleRequest>,
    ) -> Result<Response<SimpleResponse>, Status> {
        Ok(Response::new(SimpleResponse {
            payload: Some(Payload {
                r#type: PayloadType::Compressable as i32,
                body: vec![0; request.into_inner().response_size as usize],
            }),
            ..Default::default()
        }))
    }

    type StreamingCallStream =
        Pin<Box<dyn Stream<Item = Result<SimpleResponse, Status>> + Send + 'static>>;

    async fn streaming_call(
        &self,
        request: Request<tonic::Streaming<SimpleRequest>>,
    ) -> Result<Response<Self::StreamingCallStream>, Status> {
        let mut inbound = request.into_inner();

        let output = async_stream::try_stream! {
            while let Some(simple_request) = inbound.next().await {
                let request = simple_request?;
                yield SimpleResponse {
                    payload: Some(Payload {
                        r#type: PayloadType::Compressable as i32,
                        body: vec![0; request.response_size as usize],
                    }),
                    ..Default::default()
                };
            }
        };

        Ok(Response::new(Box::pin(output) as Self::StreamingCallStream))
    }

    async fn streaming_from_client(
        &self,
        _request: tonic::Request<tonic::Streaming<SimpleRequest>>,
    ) -> Result<Response<SimpleResponse>, Status> {
        Err(Status::unimplemented("method unimplemented"))
    }

    type StreamingFromServerStream =
        Pin<Box<dyn Stream<Item = Result<SimpleResponse, Status>> + Send + 'static>>;

    async fn streaming_from_server(
        &self,
        _request: Request<SimpleRequest>,
    ) -> Result<Response<Self::StreamingFromServerStream>, Status> {
        Err(Status::unimplemented("method unimplemented"))
    }

    type StreamingBothWaysStream =
        Pin<Box<dyn Stream<Item = Result<SimpleResponse, Status>> + Send + 'static>>;

    async fn streaming_both_ways(
        &self,
        _request: Request<tonic::Streaming<SimpleRequest>>,
    ) -> Result<Response<Self::StreamingBothWaysStream>, Status> {
        Err(Status::unimplemented("method unimplemented"))
    }
}

impl Drop for BenchmarkServer {
    fn drop(&mut self) {
        println!("Server is being closed");
        self.cancellation_token.cancel();
    }
}
