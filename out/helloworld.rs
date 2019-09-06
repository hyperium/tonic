/// The request message containing the user's name.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct HelloRequest {
    #[prost(string, tag = "1")]
    pub name: std::string::String,
}
/// The response message containing the greetings
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct HelloReply {
    #[prost(string, tag = "1")]
    pub message: std::string::String,
}
use tonic::_codegen::*;
#[async_trait]
pub trait Greeter: Send + Sync + 'static {
    async fn say_hello(
        &self,
        request: tonic::Request<self::HelloRequest>,
    ) -> Result<tonic::Response<self::HelloReply>, tonic::Status>;
}
#[derive(Clone)]
pub struct GreeterServer<T: Greeter> {
    inner: std::sync::Arc<T>,
}
pub struct GreeterServerSvc<T: Greeter> {
    inner: std::sync::Arc<T>,
}
impl<T: Greeter> GreeterServer<T> {
    pub fn new(inner: T) -> Self {
        let inner = std::sync::Arc::new(inner);
        Self { inner }
    }
}
impl<T: Greeter> GreeterServerSvc<T> {
    pub fn new(inner: std::sync::Arc<T>) -> Self {
        Self { inner }
    }
}
impl<T: Greeter, R> Service<R> for GreeterServer<T> {
    type Response = GreeterServerSvc<T>;
    type Error = tonic::error::Never;
    type Future = Ready<Result<Self::Response, Self::Error>>;
    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn call(&mut self, _: R) -> Self::Future {
        ok(GreeterServerSvc::new(self.inner.clone()))
    }
}
impl<T: Greeter> Service<http::Request<tonic::_codegen::HyperBody>> for GreeterServerSvc<T> {
    type Response = http::Response<tonic::BoxBody>;
    type Error = tonic::error::Never;
    type Future = BoxFuture<Self::Response, Self::Error>;
    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn call(&mut self, req: http::Request<tonic::_codegen::HyperBody>) -> Self::Future {
        let inner = self.inner.clone();
        match req.uri().path() {
            "/helloworld.Greeter/SayHello" => {
                struct SayHello<T: Greeter>(pub std::sync::Arc<T>);
                impl<T: Greeter> tonic::server::UnaryService<self::HelloRequest> for SayHello<T> {
                    type Response = self::HelloReply;
                    type Future = BoxFuture<tonic::Response<Self::Response>, tonic::Status>;
                    fn call(
                        &mut self,
                        request: tonic::Request<self::HelloRequest>,
                    ) -> Self::Future {
                        let inner = self.0.clone();
                        let fut = async move { inner.say_hello(request).await };
                        Box::pin(fut)
                    }
                }
                let inner = self.inner.clone();
                let fut = async move {
                    let method = SayHello(inner);
                    let codec = tonic::codec::ProstCodec::new();
                    let mut grpc = tonic::server::Grpc::new(codec);
                    let res = grpc.unary(method, req).await;
                    Ok(res)
                };
                Box::pin(fut)
            }
            _ => unimplemented!("use grpc unimplemented"),
        }
    }
}
use tonic::_codegen::*;
pub struct GreeterClient<T> {
    inner: tonic::client::Grpc<T>,
}
impl<T> GreeterClient<T>
where
    T: tonic::client::GrpcService<tonic::BoxBody>,
    T::ResponseBody: tonic::body::Body + tonic::_codegen::HttpBody + Send + 'static,
    T::Error: Into<tonic::error::Error>,
    <T::ResponseBody as tonic::_codegen::HttpBody>::Error: Into<tonic::error::Error> + Send,
    <T::ResponseBody as tonic::_codegen::HttpBody>::Data: Into<bytes::Bytes> + Send,
{
    pub fn new(inner: T) -> Self {
        let inner = tonic::client::Grpc::new(inner);
        Self { inner }
    }
    pub async fn ready(&mut self) -> Result<(), tonic::Status> {
        self.inner.ready().await.map_err(|e| {
            tonic::Status::new(
                tonic::Code::Unknown,
                format!("Service was not ready: {}", e.into()),
            )
        })
    }
    pub async fn say_hello(
        &mut self,
        request: tonic::Request<self::HelloRequest>,
    ) -> Result<tonic::Response<self::HelloReply>, tonic::Status> {
        self.ready().await?;
        let codec = tonic::codec::ProstCodec::new();
        let path = http::uri::PathAndQuery::from_static("/helloworld.Greeter/SayHello");
        self.inner.unary(request, path, codec).await
    }
}
impl<T: Clone> Clone for GreeterClient<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}
