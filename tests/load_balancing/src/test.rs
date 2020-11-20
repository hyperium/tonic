use futures::future::FutureExt;
use hyper::{Body, Request, Response};
use tokio::net::TcpListener;
use tonic::{
    body::BoxBody,
    transport::{server::Server, NamedService, ServerTlsConfig},
};
use tower_service::Service;

/// Manages construction and destruction of a tonic gRPC server for testing.
pub struct TestServer {
    shutdown_handle: Option<tokio::sync::oneshot::Sender<()>>,
    server_addr: String,
    server_future:
        Option<tokio::task::JoinHandle<std::result::Result<(), tonic::transport::Error>>>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // Gracefully shutdown the gRPC Server.
        if let Some(sender) = self.shutdown_handle.take() {
            let _res = sender.send(());
        }
    }
}

impl TestServer {
    /// Bootstrap a tonic `TestServer`, with the provided `Service`.
    ///
    /// This function will run the server asynchronously, and
    /// tear it down when `Self` is dropped.
    pub async fn start<S, T: Into<Option<String>>>(
        service: S,
        address: T,
        tls: Option<ServerTlsConfig>,
    ) -> Self
    where
        S: Service<Request<Body>, Response = Response<BoxBody>>
            + NamedService
            + Clone
            + Send
            + 'static,
        S::Future: Send + 'static,
        S::Error: Into<Box<dyn std::error::Error + Send + Sync>> + Send,
    {
        let (shutdown_handle, shutdown) = tokio::sync::oneshot::channel::<()>();

        let listener =
            TcpListener::bind(address.into().unwrap_or_else(|| "127.0.0.1:0".to_string()))
                .await
                .expect("failed to bind tcplistener");

        let server_addr = format!(
            "127.0.0.1:{}",
            listener
                .local_addr()
                .expect("failed to retrieve sockeaddr from tokio listener")
                .port()
        );
        tracing::info!("server address: {}", server_addr);

        let mut server_builder = Server::builder();

        if let Some(config) = tls {
            server_builder = server_builder
                .tls_config(config)
                .expect("failed to set tls config");
        }

        let server = server_builder.add_service(service);
        let server_future =
            tokio::spawn(server.serve_with_incoming_shutdown(listener, shutdown.map(|_| ())));

        TestServer {
            shutdown_handle: Some(shutdown_handle),
            server_addr,
            server_future: Some(server_future),
        }
    }

    /// Get the address `TestServer` is listening on.
    pub fn address(&self) -> &str {
        &self.server_addr
    }

    /// Shut the server down.
    pub async fn shutdown_sync(mut self) {
        // Gracefully shutdown the gRPC Server.
        if let Some(sender) = self.shutdown_handle.take() {
            let _res = sender.send(());
        }

        if let Some(server_future) = self.server_future.take() {
            server_future
                .await
                .expect("server did not exit gracefully")
                .expect("");
        }
    }
}
