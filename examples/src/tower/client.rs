use hello_world::greeter_client::GreeterClient;
use hello_world::HelloRequest;
use service::AuthSvc;
use tower::ServiceBuilder;

use tonic::{transport::Channel, Request, Status};

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let channel = Channel::from_static("http://[::1]:50051").connect().await?;

    let channel = ServiceBuilder::new()
        // Interceptors can be also be applied as middleware
        .layer(tonic::service::interceptor(intercept))
        .layer_fn(AuthSvc::new)
        .service(channel);

    let mut client = GreeterClient::new(channel);

    let request = tonic::Request::new(HelloRequest {
        name: "Tonic".into(),
    });

    let response = client.say_hello(request).await?;

    println!("RESPONSE={:?}", response);

    Ok(())
}

// An interceptor function.
fn intercept(req: Request<()>) -> Result<Request<()>, Status> {
    println!("received {:?}", req);
    Ok(req)
}

mod service {
    use http::{Request, Response};
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tonic::body::BoxBody;
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
        type Response = Response<BoxBody>;
        type Error = Box<dyn std::error::Error + Send + Sync>;
        #[allow(clippy::type_complexity)]
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            self.inner.poll_ready(cx).map_err(Into::into)
        }

        fn call(&mut self, req: Request<BoxBody>) -> Self::Future {
            // This is necessary because tonic internally uses `tower::buffer::Buffer`.
            // See https://github.com/tower-rs/tower/issues/547#issuecomment-767629149
            // for details on why this is necessary
            let clone = self.inner.clone();
            let mut inner = std::mem::replace(&mut self.inner, clone);

            Box::pin(async move {
                // Do extra async work here...
                let response = inner.call(req).await?;

                Ok(response)
            })
        }
    }
}
