use hello_world::greeter_client::GreeterClient;
use hello_world::HelloRequest;
use service::AuthSvc;

use tonic::transport::Channel;

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let channel = Channel::from_static("http://[::1]:50051").connect().await?;
    let auth = AuthSvc::new(channel);

    let mut client = GreeterClient::new(auth);

    let request = tonic::Request::new(HelloRequest {
        name: "Tonic".into(),
    });

    let response = client.say_hello(request).await?;

    println!("RESPONSE={:?}", response);

    Ok(())
}

mod service {
    use http::{Request, Response};
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tonic::body::BoxBody;
    use tonic::transport::Body;
    use tonic::transport::Channel;
    use tower::Service;

    pub struct AuthSvc {
        inner: Channel,
    }

    impl AuthSvc {
        pub fn new(inner: Channel) -> Self {
            AuthSvc { inner }
        }
    }

    impl Service<Request<BoxBody>> for AuthSvc {
        type Response = Response<Body>;
        type Error = Box<dyn std::error::Error + Send + Sync>;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            self.inner.poll_ready(cx).map_err(Into::into)
        }

        fn call(&mut self, req: Request<BoxBody>) -> Self::Future {
            let mut channel = self.inner.clone();

            Box::pin(async move {
                // Do extra async work here...

                channel.call(req).await.map_err(Into::into)
            })
        }
    }
}
