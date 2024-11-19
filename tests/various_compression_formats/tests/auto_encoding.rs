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

        // Output response headers
        println!("Client response headers: {:?}", response.metadata());

        Ok(response)
    }
}

#[tokio::test]
async fn test_compression_behavior() -> Result<(), Box<dyn Error + Send + Sync>> {
    let port = "50051";
    let address = format!("http://[::1]:{}", port);

    // Создаем канал для остановки сервера
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    // Запускаем сервер
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

    // Клиент 1: запрашивает gzip
    let client1_address = address.clone();
    let client1 = async {
        let mut client =
            ClientWrapper::new(&client1_address, Some(CompressionEncoding::Gzip), None).await?;
        let response = client.send_request("Client 1".to_string()).await?;

        // Проверяем, что заголовок grpc-encoding отсутствует
        assert!(
            !response.metadata().contains_key("grpc-encoding"),
            "Expected no 'grpc-encoding' header"
        );
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    };

    // Клиент 2: не запрашивает сжатие
    let client2_address = address.clone();
    let client2 = async {
        let mut client = ClientWrapper::new(&client2_address, None, None).await?;
        let response = client.send_request("Client 2".to_string()).await?;

        // Проверяем, что заголовок grpc-encoding отсутствует
        assert!(
            !response.metadata().contains_key("grpc-encoding"),
            "Expected no 'grpc-encoding' header"
        );
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    };

    tokio::try_join!(client1, client2)?;

    // Останавливаем сервер
    shutdown_tx.send(()).unwrap();
    server_handle.await?;

    // Запускаем второй сервер с send_compressed(CompressionEncoding::Zstd)
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

    // Клиент 3: запрашивает Zstd
    let client3_address = address.clone();
    let client3 = async {
        let mut client =
            ClientWrapper::new(&client3_address, Some(CompressionEncoding::Zstd), None).await?;
        let response = client.send_request("Client 3".to_string()).await?;

        // Проверяем, что заголовок grpc-encoding установлен
        let grpc_encoding = response
            .metadata()
            .get("grpc-encoding")
            .expect("Missing 'grpc-encoding' header");
        assert_eq!(grpc_encoding, "zstd");
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    };

    // Клиент 4: запрашивает Gzip, который не поддерживается сервером
    let client4_address = address.clone();
    let client4 = async {
        let mut client =
            ClientWrapper::new(&client4_address, Some(CompressionEncoding::Gzip), None).await?;
        let response = client.send_request("Client 4".to_string()).await?;

        // Поскольку сервер не поддерживает Gzip, заголовок grpc-encoding должен отсутствовать
        assert!(
            !response.metadata().contains_key("grpc-encoding"),
            "Expected no 'grpc-encoding' header"
        );
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    };

    tokio::try_join!(client3, client4)?;

    // Останавливаем сервер
    shutdown_tx2.send(()).unwrap();
    server_handle2.await?;

    Ok(())
}
