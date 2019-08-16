use crate::{
    body::{BoxAsyncBody, BytesBuf},
    codec::{decode, encode, Codec, Streaming},
    server::{ClientStreamingService, ServerStreamingService, StreamingService, UnaryService},
    Code, Request, Response, Status,
};
use futures_core::{Stream, TryStream};
use futures_util::{future, stream, TryStreamExt};
use http_body::Body;
use std::pin::Pin;

type BoxStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send + 'static>>;

pub struct Grpc<T> {
    codec: T,
}

impl<T> Grpc<T>
where
    T: Codec,
    T::Decoder: Send + 'static,
    T::Decode: Send + Unpin + 'static,
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
        B: Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<crate::Error> + Send,
    {
        let request = match self.map_request_unary(req).await {
            Ok(r) => r,
            Err(status) => {
                return self
                    .map_response::<stream::Once<future::Ready<Result<T::Encode, Status>>>>(Err(
                        status,
                    ))
                    .map(BoxAsyncBody::new_try);
            }
        };

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
        B: Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<crate::Error> + Send,
    {
        let request = match self.map_request_unary(req).await {
            Ok(r) => r,
            Err(status) => {
                return self
                    .map_response::<S::ResponseStream>(Err(status))
                    .map(BoxAsyncBody::new_try);
            }
        };

        let response = service.call(request).await;

        self.map_response(response).map(BoxAsyncBody::new_try)
    }

//BoxStream<T::Decode>,
    pub async fn client_streaming<S, B>(
        &mut self,
        mut service: S,
        req: http::Request<B>,
    ) -> http::Response<BoxAsyncBody>
    where
        S: ClientStreamingService<Streaming<T::Decode>, Response = T::Encode>,
        T::Decode: Send + 'static,
        T::Decoder: Send + 'static,
        B: Body + Send + 'static,
        B::Data: Send + 'static,
        B::Error: Into<crate::Error> + Send + 'static,
    {
        let request = self.map_request_streaming(req);
        let response = service
            .call(request)
            .await
            .map(|r| r.map(|m| stream::once(future::ok(m))));
        self.map_response(response).map(BoxAsyncBody::new_try)
    }

    pub async fn streaming<S, B>(
        &mut self,
        mut service: S,
        req: http::Request<B>,
    ) -> http::Response<BoxAsyncBody>
    where
        S: StreamingService<Streaming<T::Decode>, Response = T::Encode> + Send,
        S::ResponseStream: Send + 'static,
        B: Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<crate::Error> + Send,
    {
        let request = self.map_request_streaming(req);
        let response = service.call(request).await;
        self.map_response(response).map(BoxAsyncBody::new_try)
    }

    async fn map_request_unary<B>(
        &mut self,
        request: http::Request<B>,
    ) -> Result<Request<T::Decode>, Status>
    where
        B: Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<crate::Error> + Send,
    {
        let (parts, body) = request.into_parts();
        let stream = decode(self.codec.decoder(), body).into_stream();

        futures_util::pin_mut!(stream);

        let message = stream
            .try_next()
            .await?
            .ok_or(Status::new(Code::Internal, "Missing request message."))?;

        Ok(Request::from_http_parts(parts, message))
    }

    fn map_request_streaming<B>(
        &mut self,
        request: http::Request<B>,
    ) -> Request<Streaming<T::Decode>>
    where
        B: Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<crate::Error> + Send,
    {
        Request::from_http(
            request.map(|b| {
                Streaming::new(decode(self.codec.decoder(), b).into_stream())
            }),
        )
    }

    fn map_response<B>(
        &mut self,
        response: Result<crate::Response<B>, Status>,
    ) -> http::Response<BoxStream<BytesBuf>>
    where
        B: TryStream<Ok = T::Encode, Error = Status> + Send + 'static,
    {
        match response {
            Ok(r) => {
                let (mut parts, body) = r.into_http().into_parts();

                // Set the content type
                parts.headers.insert(
                    http::header::CONTENT_TYPE,
                    http::header::HeaderValue::from_static(T::CONTENT_TYPE),
                );

                // TODO: find way to pin this to the stack instead
                let body = Box::pin(body.into_stream());
                let body = encode(self.codec.encoder(), body).into_stream();

                let body = Box::pin(body) as BoxStream<BytesBuf>;
                http::Response::from_parts(parts, body)
            }
            Err(status) => {
                let status = stream::once(future::err(status));
                let body = encode(self.codec.encoder(), status).into_stream();
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
