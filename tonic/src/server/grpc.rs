use crate::{
    body::BoxBody,
    codec::{encode_server, Codec, Streaming},
    server::{ClientStreamingService, ServerStreamingService, StreamingService, UnaryService},
    Code, Request, Response, Status,
};
use bytes::Bytes;
use futures_core::TryStream;
use futures_util::{future, stream, TryStreamExt};
use http_body::Body;
use std::fmt;

/// A gRPC Server handler.
///
/// This will wrap some inner [`Codec`] and provide utilities to handle
/// inbound unary, client side streaming, server side streaming, and
/// bi-directional streaming.
///
/// Each request handler method accepts some service that implements the
/// corresponding service trait and a http request that contains some body that
/// implements some [`Body`].
pub struct Grpc<T> {
    codec: T,
}

impl<T> Grpc<T>
where
    T: Codec,
{
    /// Creates a new gRPC client with the provided [`Codec`].
    pub fn new(codec: T) -> Self {
        Self { codec }
    }

    /// Handle a single unary gRPC request.
    pub async fn unary<S, B>(
        &mut self,
        mut service: S,
        req: http::Request<B>,
    ) -> http::Response<BoxBody>
    where
        S: UnaryService<T::Decode, Response = T::Encode>,
        B: Body + Send + 'static,
        B::Data: Into<Bytes> + Send,
        B::Error: Into<crate::Error> + Send,
    {
        let request = match self.map_request_unary(req).await {
            Ok(r) => r,
            Err(status) => {
                return self
                    .map_response::<stream::Once<future::Ready<Result<T::Encode, Status>>>>(Err(
                        status,
                    ));
            }
        };

        let response = service
            .call(request)
            .await
            .map(|r| r.map(|m| stream::once(future::ok(m))));

        self.map_response(response)
    }

    /// Handle a server side streaming request.
    pub async fn server_streaming<S, B>(
        &mut self,
        mut service: S,
        req: http::Request<B>,
    ) -> http::Response<BoxBody>
    where
        S: ServerStreamingService<T::Decode, Response = T::Encode>,
        S::ResponseStream: Send + 'static,
        B: Body + Send + 'static,
        B::Data: Into<Bytes> + Send,
        B::Error: Into<crate::Error> + Send,
    {
        let request = match self.map_request_unary(req).await {
            Ok(r) => r,
            Err(status) => {
                return self.map_response::<S::ResponseStream>(Err(status));
            }
        };

        let response = service.call(request).await;

        self.map_response(response)
    }

    /// Handle a client side streaming gRPC request.
    pub async fn client_streaming<S, B>(
        &mut self,
        mut service: S,
        req: http::Request<B>,
    ) -> http::Response<BoxBody>
    where
        S: ClientStreamingService<T::Decode, Response = T::Encode>,
        B: Body + Send + 'static,
        B::Data: Into<Bytes> + Send + 'static,
        B::Error: Into<crate::Error> + Send + 'static,
    {
        let request = self.map_request_streaming(req);
        let response = service
            .call(request)
            .await
            .map(|r| r.map(|m| stream::once(future::ok(m))));
        self.map_response(response)
    }

    /// Handle a bi-directional streaming gRPC request.
    pub async fn streaming<S, B>(
        &mut self,
        mut service: S,
        req: http::Request<B>,
    ) -> http::Response<BoxBody>
    where
        S: StreamingService<T::Decode, Response = T::Encode> + Send,
        S::ResponseStream: Send + 'static,
        B: Body + Send + 'static,
        B::Data: Into<Bytes> + Send,
        B::Error: Into<crate::Error> + Send,
    {
        let request = self.map_request_streaming(req);
        let response = service.call(request).await;
        self.map_response(response)
    }

    async fn map_request_unary<B>(
        &mut self,
        request: http::Request<B>,
    ) -> Result<Request<T::Decode>, Status>
    where
        B: Body + Send + 'static,
        B::Data: Into<Bytes> + Send,
        B::Error: Into<crate::Error> + Send,
    {
        let (parts, body) = request.into_parts();
        let stream = Streaming::new_request(self.codec.decoder(), body);

        futures_util::pin_mut!(stream);

        let message = stream
            .try_next()
            .await?
            .ok_or_else(|| Status::new(Code::Internal, "Missing request message."))?;

        let mut req = Request::from_http_parts(parts, message);

        if let Some(trailers) = stream.trailers().await? {
            req.metadata_mut().merge(trailers);
        }

        Ok(req)
    }

    fn map_request_streaming<B>(
        &mut self,
        request: http::Request<B>,
    ) -> Request<Streaming<T::Decode>>
    where
        B: Body + Send + 'static,
        B::Data: Into<Bytes> + Send,
        B::Error: Into<crate::Error> + Send,
    {
        Request::from_http(request.map(|body| Streaming::new_request(self.codec.decoder(), body)))
    }

    fn map_response<B>(
        &mut self,
        response: Result<crate::Response<B>, Status>,
    ) -> http::Response<BoxBody>
    where
        B: TryStream<Ok = T::Encode, Error = Status> + Send + 'static,
    {
        match response {
            Ok(r) => {
                let (mut parts, body) = r.into_http().into_parts();

                // Set the content type
                parts.headers.insert(
                    http::header::CONTENT_TYPE,
                    http::header::HeaderValue::from_static("application/grpc"),
                );

                let body = encode_server(self.codec.encoder(), body.into_stream());

                http::Response::from_parts(parts, BoxBody::new(body))
            }
            Err(status) => {
                let (mut parts, _body) = Response::new(()).into_http().into_parts();

                parts.headers.insert(
                    http::header::CONTENT_TYPE,
                    http::header::HeaderValue::from_static("application/grpc"),
                );

                status.add_header(&mut parts.headers).unwrap();

                http::Response::from_parts(parts, BoxBody::empty())
            }
        }
    }
}

impl<T> fmt::Debug for Grpc<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Grpc").finish()
    }
}
