use std::error::Error;
use tokio::sync::oneshot;
use tonic::codegen::CompressionEncoding;
use tonic::transport::{Channel, Server};
use tonic::{Request, Response, Status};

use various_compression_formats::proto_box::{
    proto_service_client::ProtoServiceClient,
    proto_service_server::{ProtoService, ProtoServiceServer},
    Input, Output,
};

#[derive(Default)]
pub struct ServerTest;

#[tonic::async_trait]
impl ProtoService for ServerTest {
    async fn rpc(&self, request: Request<Input>) -> Result<Response<Output>, Status> {
        println!("Server received request: {:?}", request);

        let response = Output {
            data: format!("Received: {}", request.into_inner().data),
        };

        Ok(Response::new(response))
    }
}

struct ClientWrapper {
    client: ProtoServiceClient<Channel>,
}

impl ClientWrapper {
    async fn new(
        address: &str,
        accept: Option<CompressionEncoding>,
        send: Option<CompressionEncoding>,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let channel = Channel::from_shared(address.to_string())?.connect().await?;
        let mut client = ProtoServiceClient::new(channel);

        if let Some(encoding) = accept {
            client = client.accept_compressed(encoding);
        }

        if let Some(encoding) = send {
            client = client.send_compressed(encoding);
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

        println!("Client received response: {:?}", response);

        println!("Client response headers: {:?}", response.metadata());

        Ok(response)
    }
}

#[tokio::test]
async fn test_compression_behavior() -> Result<(), Box<dyn Error + Send + Sync>> {
    let port = "50051";
    let address = format!("http://[::1]:{}", port);

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let server_handle = tokio::spawn(async move {
        let srv = ServerTest::default();
        println!("Starting server on port {}", port);

        Server::builder()
            .add_service(ProtoServiceServer::new(srv))
            .serve_with_shutdown(
                format!("[::1]:{}", port)
                    .parse()
                    .expect("Failed to parse address"),
                async {
                    shutdown_rx.await.ok();
                },
            )
            .await
            .expect("Server crashed");
    });

    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    // Client 1: Requests Gzip
    let client1_address = address.clone();
    let client1 = async {
        let mut client =
            ClientWrapper::new(&client1_address, Some(CompressionEncoding::Gzip), None).await?;
        let response = client.send_request("Client 1".to_string()).await?;

        // Checking that the rpc-encoding header is missing
        assert!(
            !response.metadata().contains_key("grpc-encoding"),
            "Expected no 'grpc-encoding' header"
        );
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    };

    // Client 2: does not request compression
    let client2_address = address.clone();
    let client2 = async {
        let mut client = ClientWrapper::new(&client2_address, None, None).await?;
        let response = client.send_request("Client 2".to_string()).await?;

        // Checking that the rpc-encoding header is missing
        assert!(
            !response.metadata().contains_key("grpc-encoding"),
            "Expected no 'grpc-encoding' header"
        );
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    };

    tokio::try_join!(client1, client2)?;

    shutdown_tx.send(()).unwrap();
    server_handle.await?;

    // Starting the second server with send_compressed(CompressionEncoding::Zstd)
    let (shutdown_tx2, shutdown_rx2) = oneshot::channel::<()>();

    let server_handle2 = tokio::spawn(async move {
        let srv = ServerTest::default();
        println!("Starting server on port {} with Zstd compression", port);

        Server::builder()
            .add_service(ProtoServiceServer::new(srv).send_compressed(CompressionEncoding::Zstd))
            .serve_with_shutdown(
                format!("[::1]:{}", port)
                    .parse()
                    .expect("Failed to parse address"),
                async {
                    shutdown_rx2.await.ok();
                },
            )
            .await
            .expect("Server crashed");
    });

    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    // Client 3: Requests Zstd
    let client3_address = address.clone();
    let client3 = async {
        let mut client =
            ClientWrapper::new(&client3_address, Some(CompressionEncoding::Zstd), None).await?;
        let response = client.send_request("Client 3".to_string()).await?;

        // Check that the rpc-encoding header is set
        let grpc_encoding = response
            .metadata()
            .get("grpc-encoding")
            .expect("Missing 'grpc-encoding' header");
        assert_eq!(grpc_encoding, "zstd");
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    };

    // Client 4: Requests Gzip, which is not supported by the server
    let client4_address = address.clone();
    let client4 = async {
        let mut client =
            ClientWrapper::new(&client4_address, Some(CompressionEncoding::Gzip), None).await?;
        let response = client.send_request("Client 4".to_string()).await?;

        // Since the server does not support Gzip, the grpc-encoding header should be omitted
        assert!(
            !response.metadata().contains_key("grpc-encoding"),
            "Expected no 'grpc-encoding' header"
        );
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    };

    tokio::try_join!(client3, client4)?;

    shutdown_tx2.send(()).unwrap();
    server_handle2.await?;

    Ok(())
}

#[tokio::test]
async fn test_auto_encoding_behavior() -> Result<(), Box<dyn Error + Send + Sync>> {
    let port = "50052";
    let address = format!("http://[::1]:{}", port);

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    // Starting the server with auto_encoding
    let server_handle = tokio::spawn(async move {
        let srv = ServerTest::default();
        println!("Starting server on port {} with auto_encoding", port);

        Server::builder()
            .add_service(
                ProtoServiceServer::new(srv)
                    .accept_compressed(CompressionEncoding::Gzip)
                    .accept_compressed(CompressionEncoding::Zstd)
                    .auto_encoding(),
            )
            .serve_with_shutdown(
                format!("[::1]:{}", port)
                    .parse()
                    .expect("Failed to parse address"),
                async {
                    shutdown_rx.await.ok();
                },
            )
            .await
            .expect("Server crashed");
    });

    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    // Client 5: Requests Gzip
    let client5_address = address.clone();
    let client5 = async {
        let mut client =
            ClientWrapper::new(&client5_address, Some(CompressionEncoding::Gzip), None).await?;
        let response = client.send_request("Client 5".to_string()).await?;

        // Check that the grpc-encoding header is set to gzip
        let grpc_encoding = response
            .metadata()
            .get("grpc-encoding")
            .expect("Missing 'grpc-encoding' header");
        assert_eq!(grpc_encoding, "gzip");
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    };

    // Client 6: Requests Zstd
    let client6_address = address.clone();
    let client6 = async {
        let mut client =
            ClientWrapper::new(&client6_address, Some(CompressionEncoding::Zstd), None).await?;
        let response = client.send_request("Client 6".to_string()).await?;

         // Check that the rpc-encoding header is set to zstd
        let grpc_encoding = response
            .metadata()
            .get("grpc-encoding")
            .expect("Missing 'grpc-encoding' header");
        assert_eq!(grpc_encoding, "zstd");
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    };

    tokio::try_join!(client5, client6)?;

    shutdown_tx.send(()).unwrap();
    server_handle.await?;

    Ok(())
}
