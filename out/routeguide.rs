/// Points are represented as latitude-longitude pairs in the E7 representation
/// (degrees multiplied by 10**7 and rounded to the nearest integer).
/// Latitudes should be in the range +/- 90 degrees and longitude should be in
/// the range +/- 180 degrees (inclusive).
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Point {
    #[prost(int32, tag = "1")]
    pub latitude: i32,
    #[prost(int32, tag = "2")]
    pub longitude: i32,
}
/// A latitude-longitude rectangle, represented as two diagonally opposite
/// points "lo" and "hi".
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Rectangle {
    /// One corner of the rectangle.
    #[prost(message, optional, tag = "1")]
    pub lo: ::std::option::Option<Point>,
    /// The other corner of the rectangle.
    #[prost(message, optional, tag = "2")]
    pub hi: ::std::option::Option<Point>,
}
/// A feature names something at a given point.
///
/// If a feature could not be named, the name is empty.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Feature {
    /// The name of the feature.
    #[prost(string, tag = "1")]
    pub name: std::string::String,
    /// The point where the feature is detected.
    #[prost(message, optional, tag = "2")]
    pub location: ::std::option::Option<Point>,
}
/// A RouteNote is a message sent while at a given point.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RouteNote {
    /// The location from which the message is sent.
    #[prost(message, optional, tag = "1")]
    pub location: ::std::option::Option<Point>,
    /// The message to be sent.
    #[prost(string, tag = "2")]
    pub message: std::string::String,
}
/// A RouteSummary is received in response to a RecordRoute rpc.
///
/// It contains the number of individual points received, the number of
/// detected features, and the total distance covered as the cumulative sum of
/// the distance between each point.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RouteSummary {
    /// The number of points received.
    #[prost(int32, tag = "1")]
    pub point_count: i32,
    /// The number of known features passed while traversing the route.
    #[prost(int32, tag = "2")]
    pub feature_count: i32,
    /// The distance covered in metres.
    #[prost(int32, tag = "3")]
    pub distance: i32,
    /// The duration of the traversal in seconds.
    #[prost(int32, tag = "4")]
    pub elapsed_time: i32,
}
use tonic::_codegen::*;
#[async_trait]
pub trait RouteGuide: Send + Sync + 'static {
    async fn get_feature(
        &self,
        request: tonic::Request<self::Point>,
    ) -> Result<tonic::Response<self::Feature>, tonic::Status>;
    type ListFeaturesStream: Stream<Item = Result<self::Feature, tonic::Status>>
        + Unpin
        + Send
        + 'static;
    async fn list_features(
        &self,
        request: tonic::Request<self::Rectangle>,
    ) -> Result<tonic::Response<Self::ListFeaturesStream>, tonic::Status>;
    async fn record_route(
        &self,
        request: tonic::Request<tonic::Streaming<self::Point>>,
    ) -> Result<tonic::Response<self::RouteSummary>, tonic::Status>;
    type RouteChatStream: Stream<Item = Result<self::RouteNote, tonic::Status>>
        + Unpin
        + Send
        + 'static;
    async fn route_chat(
        &self,
        request: tonic::Request<tonic::Streaming<self::RouteNote>>,
    ) -> Result<tonic::Response<Self::RouteChatStream>, tonic::Status>;
}
#[derive(Clone)]
pub struct RouteGuideServer<T: RouteGuide> {
    inner: std::sync::Arc<T>,
}
pub struct RouteGuideServerSvc<T: RouteGuide> {
    inner: std::sync::Arc<T>,
}
impl<T: RouteGuide> RouteGuideServer<T> {
    pub fn new(inner: T) -> Self {
        let inner = std::sync::Arc::new(inner);
        Self { inner }
    }
}
impl<T: RouteGuide> RouteGuideServerSvc<T> {
    pub fn new(inner: std::sync::Arc<T>) -> Self {
        Self { inner }
    }
}
impl<T: RouteGuide, R> Service<R> for RouteGuideServer<T> {
    type Response = RouteGuideServerSvc<T>;
    type Error = tonic::error::Never;
    type Future = Ready<Result<Self::Response, Self::Error>>;
    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn call(&mut self, _: R) -> Self::Future {
        ok(RouteGuideServerSvc::new(self.inner.clone()))
    }
}
impl<T: RouteGuide> Service<http::Request<tonic::_codegen::HyperBody>> for RouteGuideServerSvc<T> {
    type Response = http::Response<tonic::BoxBody>;
    type Error = tonic::error::Never;
    type Future = BoxFuture<Self::Response, Self::Error>;
    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn call(&mut self, req: http::Request<tonic::_codegen::HyperBody>) -> Self::Future {
        let inner = self.inner.clone();
        match req.uri().path() {
            "/routeguide.RouteGuide/GetFeature" => {
                struct GetFeature<T: RouteGuide>(pub std::sync::Arc<T>);
                impl<T: RouteGuide> tonic::server::UnaryService<self::Point> for GetFeature<T> {
                    type Response = self::Feature;
                    type Future = BoxFuture<tonic::Response<Self::Response>, tonic::Status>;
                    fn call(&mut self, request: tonic::Request<self::Point>) -> Self::Future {
                        let inner = self.0.clone();
                        let fut = async move { inner.get_feature(request).await };
                        Box::pin(fut)
                    }
                }
                let inner = self.inner.clone();
                let fut = async move {
                    let method = GetFeature(inner);
                    let codec = tonic::codec::ProstCodec::new();
                    let mut grpc = tonic::server::Grpc::new(codec);
                    let res = grpc.unary(method, req).await;
                    Ok(res)
                };
                Box::pin(fut)
            }
            "/routeguide.RouteGuide/ListFeatures" => {
                struct ListFeatures<T: RouteGuide>(pub std::sync::Arc<T>);
                impl<T: RouteGuide> tonic::server::ServerStreamingService<self::Rectangle> for ListFeatures<T> {
                    type Response = self::Feature;
                    type ResponseStream = T::ListFeaturesStream;
                    type Future = BoxFuture<tonic::Response<Self::ResponseStream>, tonic::Status>;
                    fn call(&mut self, request: tonic::Request<self::Rectangle>) -> Self::Future {
                        let inner = self.0.clone();
                        let fut = async move { inner.list_features(request).await };
                        Box::pin(fut)
                    }
                }
                let inner = self.inner.clone();
                let fut = async move {
                    let method = ListFeatures(inner);
                    let codec = tonic::codec::ProstCodec::new();
                    let mut grpc = tonic::server::Grpc::new(codec);
                    let res = grpc.server_streaming(method, req).await;
                    Ok(res)
                };
                Box::pin(fut)
            }
            "/routeguide.RouteGuide/RecordRoute" => {
                struct RecordRoute<T: RouteGuide>(pub std::sync::Arc<T>);
                impl<T: RouteGuide> tonic::server::ClientStreamingService<self::Point> for RecordRoute<T> {
                    type Response = self::RouteSummary;
                    type Future = BoxFuture<tonic::Response<Self::Response>, tonic::Status>;
                    fn call(
                        &mut self,
                        request: tonic::Request<tonic::Streaming<self::Point>>,
                    ) -> Self::Future {
                        let inner = self.0.clone();
                        let fut = async move { inner.record_route(request).await };
                        Box::pin(fut)
                    }
                }
                let inner = self.inner.clone();
                let fut = async move {
                    let method = RecordRoute(inner);
                    let codec = tonic::codec::ProstCodec::new();
                    let mut grpc = tonic::server::Grpc::new(codec);
                    let res = grpc.client_streaming(method, req).await;
                    Ok(res)
                };
                Box::pin(fut)
            }
            "/routeguide.RouteGuide/RouteChat" => {
                struct RouteChat<T: RouteGuide>(pub std::sync::Arc<T>);
                impl<T: RouteGuide> tonic::server::StreamingService<self::RouteNote> for RouteChat<T> {
                    type Response = self::RouteNote;
                    type ResponseStream = T::RouteChatStream;
                    type Future = BoxFuture<tonic::Response<Self::ResponseStream>, tonic::Status>;
                    fn call(
                        &mut self,
                        request: tonic::Request<tonic::Streaming<self::RouteNote>>,
                    ) -> Self::Future {
                        let inner = self.0.clone();
                        let fut = async move { inner.route_chat(request).await };
                        Box::pin(fut)
                    }
                }
                let inner = self.inner.clone();
                let fut = async move {
                    let method = RouteChat(inner);
                    let codec = tonic::codec::ProstCodec::new();
                    let mut grpc = tonic::server::Grpc::new(codec);
                    let res = grpc.streaming(method, req).await;
                    Ok(res)
                };
                Box::pin(fut)
            }
            _ => unimplemented!("use grpc unimplemented"),
        }
    }
}
use tonic::_codegen::*;
pub struct RouteGuideClient<T> {
    inner: tonic::client::Grpc<T>,
}
impl<T> RouteGuideClient<T>
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
    pub async fn get_feature(
        &mut self,
        request: tonic::Request<self::Point>,
    ) -> Result<tonic::Response<self::Feature>, tonic::Status> {
        self.ready().await?;
        let codec = tonic::codec::ProstCodec::new();
        let path = http::uri::PathAndQuery::from_static("/routeguide.RouteGuide/GetFeature");
        self.inner.unary(request, path, codec).await
    }
    pub async fn list_features(
        &mut self,
        request: tonic::Request<self::Rectangle>,
    ) -> Result<tonic::Response<tonic::codec::Streaming<self::Feature>>, tonic::Status> {
        self.ready().await?;
        let codec = tonic::codec::ProstCodec::new();
        let path = http::uri::PathAndQuery::from_static("/routeguide.RouteGuide/ListFeatures");
        self.inner.server_streaming(request, path, codec).await
    }
    pub async fn record_route<S>(
        &mut self,
        request: tonic::Request<S>,
    ) -> Result<tonic::Response<self::RouteSummary>, tonic::Status>
    where
        S: tonic::_codegen::Stream<Item = Result<self::Point, tonic::Status>> + Send + 'static,
    {
        self.ready().await?;
        let codec = tonic::codec::ProstCodec::new();
        let path = http::uri::PathAndQuery::from_static("/routeguide.RouteGuide/RecordRoute");
        let request = request.map(|s| Box::pin(s));
        self.inner.client_streaming(request, path, codec).await
    }
    pub async fn route_chat<S>(
        &mut self,
        request: tonic::Request<S>,
    ) -> Result<tonic::Response<tonic::codec::Streaming<self::RouteNote>>, tonic::Status>
    where
        S: tonic::_codegen::Stream<Item = Result<self::RouteNote, tonic::Status>> + Send + 'static,
    {
        self.ready().await?;
        let codec = tonic::codec::ProstCodec::new();
        let path = http::uri::PathAndQuery::from_static("/routeguide.RouteGuide/RouteChat");
        let request = request.map(|s| Box::pin(s));
        self.inner.streaming(request, path, codec).await
    }
}
impl<T: Clone> Clone for RouteGuideClient<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}
