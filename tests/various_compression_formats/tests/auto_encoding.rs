use std::error::Error;

use tokio::net::TcpListener;
use tokio::sync::oneshot;

use tonic::codegen::CompressionEncoding;
use tonic::transport::{server::TcpIncoming, Channel, Server};
use tonic::{Request, Response, Status};

use various_compression_formats::proto_box::{
    proto_service_client::ProtoServiceClient,
    proto_service_server::{ProtoService, ProtoServiceServer},
    Input, Output,
};

const LOCALHOST: &str = "127.0.0.1:0";

#[derive(Default)]
pub struct ServerTest;

#[tonic::async_trait]
impl ProtoService for ServerTest {
    async fn rpc(&self, request: Request<Input>) -> Result<Response<Output>, Status> {
        println!("Server received request: {:?}", request);

        Ok(Response::new(Output {
            data: format!("Received: {}", request.into_inner().data),
        }))
    }
}

struct ClientWrapper {
    client: ProtoServiceClient<Channel>,
}

impl ClientWrapper {
    async fn new(
        address: &str,
        accept: Option<CompressionEncoding>,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let channel = Channel::from_shared(address.to_string())?.connect().await?;
        let mut client = ProtoServiceClient::new(channel);

        if let Some(encoding) = accept {
            client = client.accept_compressed(encoding);
        }

        Ok(Self { client })
    }

    async fn send_request(
        &mut self,
        data: String,
    ) -> Result<Response<Output>, Box<dyn Error + Send + Sync>> {
        let request = Request::new(Input { data });

        println!("Client sending request: {:?}", request);

        let response = self.client.rpc(request).await?;

        println!("Client response headers: {:?}", response.metadata());

        Ok(response)
    }
}

async fn start_server(
    listener: TcpListener,
    send: Option<CompressionEncoding>,
    auto: bool,
) -> oneshot::Sender<()> {
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let srv = ServerTest::default();
    let mut service = ProtoServiceServer::new(srv);

    if let Some(encoding) = send {
        service = service.send_compressed(encoding);
    }

    if auto {
        service = service.auto_encoding();
    }

    let server = Server::builder()
        .add_service(service)
        .serve_with_incoming_shutdown(
            TcpIncoming::from_listener(listener, true, None).unwrap(),
            async {
                shutdown_rx.await.ok();
            },
        );

    tokio::spawn(async move {
        server.await.expect("Server crashed");
    });

    shutdown_tx
}

async fn run_client_test(
    address: &str,
    client_accept: Option<CompressionEncoding>,
    expected_encoding: Option<&str>,
    data: &str,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut client = ClientWrapper::new(address, client_accept).await?;
    let response = client.send_request(data.to_string()).await?;

    match expected_encoding {
        Some(encoding) => {
            let grpc_encoding = response
                .metadata()
                .get("grpc-encoding")
                .expect("Missing 'grpc-encoding' header");
            assert_eq!(grpc_encoding, encoding);
        }
        None => {
            assert!(
                !response.metadata().contains_key("grpc-encoding"),
                "Expected no 'grpc-encoding' header"
            );
        }
    }

    Ok(())
}

#[tokio::test]
async fn test_compression_behavior() -> Result<(), Box<dyn Error + Send + Sync>> {
    let listener = TcpListener::bind(LOCALHOST).await?;
    let address = format!("http://{}", listener.local_addr().unwrap());

    // The server is not specified to send data with any compression
    let shutdown_tx = start_server(listener, None, false).await;

    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    tokio::try_join!(
        // Client 1 can only accept gzip encoding or uncompressed,
        // so all data must be returned uncompressed
        run_client_test(&address, Some(CompressionEncoding::Gzip), None, "Client 1"),
        // Client 2 can only accept non-compressed data,
        // so all data must be returned uncompressed
        run_client_test(&address, None, None, "Client 2")
    )?;

    shutdown_tx.send(()).unwrap();

    let listener = TcpListener::bind(LOCALHOST).await?;
    let address = format!("http://{}", listener.local_addr().unwrap());

    // The server is specified to send data with zstd compression
    let shutdown_tx = start_server(listener, Some(CompressionEncoding::Zstd), false).await;

    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    tokio::try_join!(
        // Client 3 can only accept zstd encoding or uncompressed,
        // so all data must be returned compressed with zstd
        run_client_test(
            &address,
            Some(CompressionEncoding::Zstd),
            Some("zstd"),
            "Client 3"
        ),
        // Client 4 can only accept Gzip encoding or uncompressed,
        // so all data must be returned uncompressed
        run_client_test(&address, Some(CompressionEncoding::Gzip), None, "Client 4")
    )?;

    shutdown_tx.send(()).unwrap();

    Ok(())
}

#[tokio::test]
async fn test_auto_encoding_behavior() -> Result<(), Box<dyn Error + Send + Sync>> {
    let listener = TcpListener::bind(LOCALHOST).await?;
    let address = format!("http://{}", listener.local_addr().unwrap());

    // The server returns in the compression format that the client prefers
    let shutdown_tx = start_server(listener, Some(CompressionEncoding::Gzip), true).await;

    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    tokio::try_join!(
        // Client 5 can accept gzip encoding or uncompressed, so all data must be returned compressed with gzip
        run_client_test(
            &address,
            Some(CompressionEncoding::Gzip),
            Some("gzip"),
            "Client 5"
        ),
        // Client 6 can accept zstd encoding or uncompressed, so all data must be returned compressed with zstd
        run_client_test(
            &address,
            Some(CompressionEncoding::Zstd),
            Some("zstd"),
            "Client 6"
        )
    )?;

    shutdown_tx.send(()).unwrap();

    Ok(())
}
