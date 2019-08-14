use crate::{
    body::{BoxAsyncBody, BytesBuf},
    codec::{self, Codec},
    Request, Response, Status,
};
use futures_core::{Future, Stream, TryStream};
use futures_util::{future, stream, TryStreamExt};
use http_body::Body;
use std::pin::Pin;

type BoxStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send + 'static>>;

pub struct Grpc<T> {
    codec: T,
}

// type UnaryFuture<B> = Once<Ready<Result<B, Status>>>;
// type ResponseBody = impl Stream<Item = Result<crate::body::BytesBuf, Status>>;

pub trait UnaryService<R> {
    /// Protobuf response message type
    type Response;

    /// Response future
    type Future: Future<Output = Result<crate::Response<Self::Response>, Status>>;

    /// Call the service
    fn call(&mut self, request: Request<R>) -> Self::Future;
}

pub trait ServerStreamingService<R> {
    /// Protobuf response message type
    type Response;

    /// Stream of outbound response messages
    type ResponseStream: TryStream<Ok = Self::Response, Error = crate::Status> + Unpin;

    /// Response future
    type Future: Future<Output = Result<crate::Response<Self::ResponseStream>, crate::Status>>;

    /// Call the service
    fn call(&mut self, request: Request<R>) -> Self::Future;
}

pub trait ClientStreamingService<RequestStream> {
    /// Protobuf response message type
    type Response;

    /// Response future
    type Future: Future<Output = Result<crate::Response<Self::Response>, Status>>;

    /// Call the service
    fn call(&mut self, request: Request<RequestStream>) -> Self::Future;
}

pub trait StreamingService<RequestStream> {
    /// Protobuf response message type
    type Response;

    /// Stream of outbound response messages
    type ResponseStream: TryStream<Ok = Self::Response, Error = crate::Status> + Unpin;

    /// Response future
    type Future: Future<Output = Result<crate::Response<Self::ResponseStream>, crate::Status>>;

    /// Call the service
    fn call(&mut self, request: Request<RequestStream>) -> Self::Future;
}

impl<T> Grpc<T>
where
    T: Codec,
    T::Decode: Unpin + 'static,
    T::Encoder: Send + 'static,
    T::Encode: Send + Unpin + 'static,
{
    pub fn new(codec: T) -> Self {
        Self { codec }
    }

    pub async fn unary<S, B>(
        &mut self,
        mut service: S,
        req: http::Request<B>,
    ) -> http::Response<BoxAsyncBody>
    where
        S: UnaryService<T::Decode, Response = T::Encode>,
        B: Body,
        B::Error: Into<crate::Error>,
    {
        let (_parts, body) = req.into_parts();
        let stream = codec::decode(self.codec.decoder(), body).into_stream();
        futures_util::pin_mut!(stream);
        let message = stream.try_next().await.unwrap().unwrap();
        let request = Request::new(message);
        let response = service
            .call(request)
            .await
            .map(|r| r.map(|m| stream::once(future::ok(m))));

        self.map_response(response).map(BoxAsyncBody::new_try)
    }

    pub async fn server_streaming<S, B>(
        &mut self,
        mut service: S,
        req: http::Request<B>,
    ) -> http::Response<BoxAsyncBody>
    where
        S: ServerStreamingService<T::Decode, Response = T::Encode>,
        S::ResponseStream: Send + 'static,
        B: Body,
        B::Error: Into<crate::Error>,
    {
        let (_parts, body) = req.into_parts();
        let stream = codec::decode(self.codec.decoder(), body).into_stream();
        futures_util::pin_mut!(stream);
        let message = stream.try_next().await.unwrap().unwrap();
        let request = Request::new(message);
        let response = service.call(request).await;

        self.map_response(response).map(BoxAsyncBody::new_try)
    }

    pub async fn client_streaming<S, B>(
        &mut self,
        mut service: S,
        req: http::Request<B>,
    ) -> http::Response<impl TryStream<Ok = crate::body::BytesBuf, Error = Status>>
    where
        S: ClientStreamingService<BoxStream<T::Decode>, Response = T::Encode>,
        T::Decode: Send,
        T::Decoder: Send + 'static,
        B: Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<crate::Error> + Send,
    {
        let (_parts, body) = req.into_parts();
        let stream = codec::decode(self.codec.decoder(), body).into_stream();
        let stream = Box::pin(stream) as BoxStream<T::Decode>;
        let request = Request::new(stream);
        let response = service.call(request).await.unwrap();
        let message = response.into_inner();
        let source = stream::once(future::ok(message));
        let body = codec::encode(self.codec.encoder(), source);

        http::Response::new(body)
    }

    pub async fn streaming<S, B>(
        &mut self,
        mut service: S,
        req: http::Request<B>,
    ) -> http::Response<impl TryStream<Ok = crate::body::BytesBuf, Error = Status>>
    where
        S: StreamingService<BoxStream<T::Decode>, Response = T::Encode>,
        T::Decode: Send,
        T::Decoder: Send + 'static,
        B: Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<crate::Error> + Send,
    {
        let (_parts, body) = req.into_parts();
        let stream = codec::decode(self.codec.decoder(), body).into_stream();
        let stream = Box::pin(stream) as BoxStream<T::Decode>;
        let request = Request::new(stream);
        let response = service.call(request).await.unwrap();
        let source = response.into_inner();
        let body = codec::encode(self.codec.encoder(), source);

        http::Response::new(body)
    }

    // fn map_request<B>(&mut self, request: http::Request<B>) -> Request<B> {
    //     Request::from_http(request.map(|b| codec::decode(self.codec.decoder(), b)))
    // }

    fn map_response<B>(
        &mut self,
        response: Result<crate::Response<B>, Status>,
    ) -> http::Response<BoxStream<BytesBuf>>
    where
        B: TryStream<Ok = T::Encode, Error = Status> + Send + Unpin + 'static,
    {
        match response {
            Ok(r) => {
                let (mut parts, body) = r.into_http().into_parts();

                // Set the content type
                parts.headers.insert(
                    http::header::CONTENT_TYPE,
                    http::header::HeaderValue::from_static(T::CONTENT_TYPE),
                );

                let body = codec::encode(self.codec.encoder(), body).into_stream();

                let body = Box::pin(body) as BoxStream<BytesBuf>;
                http::Response::from_parts(parts, body)
            }
            Err(status) => {
                let status = stream::once(future::err(status));
                let body = codec::encode(self.codec.encoder(), status).into_stream();
                let (mut parts, _body) = Response::new(()).into_http().into_parts();

                parts.headers.insert(
                    http::header::CONTENT_TYPE,
                    http::header::HeaderValue::from_static(T::CONTENT_TYPE),
                );

                let body = Box::pin(body) as BoxStream<BytesBuf>;
                http::Response::from_parts(parts, body)
            }
        }
    }
}
