use hello_world::greeter_client::GreeterClient;
use hello_world::HelloRequest;
use http::Uri;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let origin = Uri::from_static("http://[::1]:50051");
    let h2c_client = h2c::H2cChannel {
        client: Client::builder(TokioExecutor::new()).build_http(),
    };

    let mut client = GreeterClient::with_origin(h2c_client, origin);

    let request = tonic::Request::new(HelloRequest {
        name: "Tonic".into(),
    });

    let response = client.say_hello(request).await?;

    println!("RESPONSE={:?}", response);

    Ok(())
}

mod h2c {
    use std::{
        pin::Pin,
        task::{Context, Poll},
    };

    use hyper::body::Incoming;
    use hyper_util::{
        client::legacy::{connect::HttpConnector, Client},
        rt::TokioExecutor,
    };
    use tonic::body::{empty_body, BoxBody};
    use tower::Service;

    pub struct H2cChannel {
        pub client: Client<HttpConnector, BoxBody>,
    }

    impl Service<http::Request<BoxBody>> for H2cChannel {
        type Response = http::Response<Incoming>;
        type Error = hyper::Error;
        type Future =
            Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: http::Request<BoxBody>) -> Self::Future {
            let client = self.client.clone();

            Box::pin(async move {
                let origin = request.uri();

                let h2c_req = hyper::Request::builder()
                    .uri(origin)
                    .header(http::header::UPGRADE, "h2c")
                    .body(empty_body())
                    .unwrap();

                let res = client.request(h2c_req).await.unwrap();

                if res.status() != http::StatusCode::SWITCHING_PROTOCOLS {
                    panic!("Our server didn't upgrade: {}", res.status());
                }

                let upgraded_io = hyper::upgrade::on(res).await.unwrap();

                // In an ideal world you would somehow cache this connection
                let (mut h2_client, conn) =
                    hyper::client::conn::http2::Builder::new(TokioExecutor::new())
                        .handshake(upgraded_io)
                        .await
                        .unwrap();
                tokio::spawn(conn);

                h2_client.send_request(request).await
            })
        }
    }
}
