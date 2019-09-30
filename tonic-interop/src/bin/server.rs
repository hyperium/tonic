use http::header::HeaderName;
use structopt::StructOpt;
use tonic::body::BoxBody;
use tonic::client::GrpcService;
use tonic::transport::{Identity, Server};
use tonic_interop::{server, MergeTrailers};

#[derive(StructOpt)]
struct Opts {
    #[structopt(long)]
    use_tls: bool,
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    tonic_interop::trace_init();

    let matches = Opts::from_args();

    let addr = "127.0.0.1:10000".parse().unwrap();

    let mut builder = Server::builder();

    if matches.use_tls {
        let cert = tokio::fs::read("tonic-interop/data/server1.pem").await?;
        let key = tokio::fs::read("tonic-interop/data/server1.key").await?;

        let identity = Identity::from_pem(cert, key);
        builder.openssl_tls(identity);
    }

    builder.interceptor_fn(|svc, req| {
        let echo_header = req
            .headers()
            .get("x-grpc-test-echo-initial")
            .map(Clone::clone);

        let echo_trailer = req
            .headers()
            .get("x-grpc-test-echo-trailing-bin")
            .map(Clone::clone)
            .map(|v| (HeaderName::from_static("x-grpc-test-echo-trailing-bin"), v));

        let call = svc.call(req);

        async move {
            let mut res = call.await?;

            if let Some(echo_header) = echo_header {
                res.headers_mut()
                    .insert("x-grpc-test-echo-initial", echo_header);
            }

            Ok(res
                .map(|b| MergeTrailers::new(b, echo_trailer))
                .map(BoxBody::new))
        }
    });

    builder
        .serve(
            addr,
            router::Router {
                test_service: std::sync::Arc::new(server::TestService),
                unimplemented_service: std::sync::Arc::new(server::UnimplementedService),
            },
        )
        .await?;

    Ok(())
}

mod router {
    use futures_util::future;
    use http::{Request, Response};
    use std::sync::Arc;
    use std::{
        future::Future,
        pin::Pin,
        task::{Context, Poll},
    };
    use tonic::{body::BoxBody, transport::Body};
    use tonic_interop::server::{
        TestService, TestServiceServer, UnimplementedService, UnimplementedServiceServer,
    };
    use tower::Service;

    #[derive(Clone)]
    pub struct Router {
        pub test_service: Arc<TestService>,
        pub unimplemented_service: Arc<UnimplementedService>,
    }

    impl Service<()> for Router {
        type Response = Router;
        type Error = Never;
        type Future = future::Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Ok(()).into()
        }

        fn call(&mut self, _req: ()) -> Self::Future {
            future::ok(self.clone())
        }
    }

    impl Service<Request<Body>> for Router {
        type Response = Response<BoxBody>;
        type Error = Never;
        type Future =
            Pin<Box<dyn Future<Output = Result<Response<BoxBody>, Never>> + Send + 'static>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Ok(()).into()
        }

        fn call(&mut self, req: Request<Body>) -> Self::Future {
            let mut segments = req.uri().path().split("/");
            segments.next();
            let service = segments.next().unwrap();

            match service {
                "grpc.testing.TestService" => {
                    let me = self.clone();
                    Box::pin(async move {
                        let mut svc = TestServiceServer::from_shared(me.test_service);
                        let mut svc = svc.call(()).await.unwrap();

                        let res = svc.call(req).await.unwrap();
                        Ok(res)
                    })
                }

                "grpc.testing.UnimplementedService" => {
                    let me = self.clone();
                    Box::pin(async move {
                        let mut svc =
                            UnimplementedServiceServer::from_shared(me.unimplemented_service);
                        let mut svc = svc.call(()).await.unwrap();

                        let res = svc.call(req).await.unwrap();
                        Ok(res)
                    })
                }

                _ => unimplemented!(),
            }
        }
    }

    #[derive(Debug)]
    pub enum Never {}

    impl std::fmt::Display for Never {
        fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match *self {}
        }
    }

    impl std::error::Error for Never {}
}
