use crate::{
    body::BoxBody,
    codec::{
        compression::{CompressionEncoding, EnabledCompressionEncodings},
        encode_server, Codec, Streaming,
    },
    server::{ClientStreamingService, ServerStreamingService, StreamingService, UnaryService},
    Code, Request, Status,
};
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
    /// Which compression encodings does the server accept for requests?
    accept_compression_encodings: EnabledCompressionEncodings,
    /// Which compression encodings might the server use for responses.
    send_compression_encodings: EnabledCompressionEncodings,
}

impl<T> Grpc<T>
where
    T: Codec,
    T::Encode: Sync,
{
    /// Creates a new gRPC server with the provided [`Codec`].
    pub fn new(codec: T) -> Self {
        Self {
            codec,
            accept_compression_encodings: EnabledCompressionEncodings::default(),
            send_compression_encodings: EnabledCompressionEncodings::default(),
        }
    }

    pub fn accept_gzip(mut self) -> Self {
        self.accept_compression_encodings.enable_gzip();
        self
    }

    pub fn send_gzip(mut self) -> Self {
        self.send_compression_encodings.enable_gzip();
        self
    }

    #[doc(hidden)]
    pub fn apply_compression_config(
        self,
        accept_encodings: EnabledCompressionEncodings,
        send_encodings: EnabledCompressionEncodings,
    ) -> Self {
        let mut this = self;

        let EnabledCompressionEncodings { gzip: accept_gzip } = accept_encodings;
        if accept_gzip {
            this = this.accept_gzip();
        }

        let EnabledCompressionEncodings { gzip: send_gzip } = send_encodings;
        if send_gzip {
            this = this.send_gzip();
        }

        this
    }

    /// Handle a single unary gRPC request.
    pub async fn unary<S, B>(
        &mut self,
        mut service: S,
        req: http::Request<B>,
    ) -> http::Response<BoxBody>
    where
        S: UnaryService<T::Decode, Response = T::Encode>,
        B: Body + Send + Sync + 'static,
        B::Error: Into<crate::Error> + Send,
    {
        let encoding = CompressionEncoding::from_accept_encoding_header(
            req.headers(),
            self.send_compression_encodings,
        );

        let request = match self.map_request_unary(req).await {
            Ok(r) => r,
            Err(status) => {
                return self
                    .map_response::<stream::Once<future::Ready<Result<T::Encode, Status>>>>(
                        Err(status),
                        encoding,
                    );
            }
        };

        let response = service
            .call(request)
            .await
            .map(|r| r.map(|m| stream::once(future::ok(m))));

        self.map_response(response, encoding)
    }

    /// Handle a server side streaming request.
    pub async fn server_streaming<S, B>(
        &mut self,
        mut service: S,
        req: http::Request<B>,
    ) -> http::Response<BoxBody>
    where
        S: ServerStreamingService<T::Decode, Response = T::Encode>,
        S::ResponseStream: Send + Sync + 'static,
        B: Body + Send + Sync + 'static,
        B::Error: Into<crate::Error> + Send,
    {
        // TODO(david): encoding

        let request = match self.map_request_unary(req).await {
            Ok(r) => r,
            Err(status) => {
                return self.map_response::<S::ResponseStream>(Err(status), None);
            }
        };

        let response = service.call(request).await;

        self.map_response(response, None)
    }

    /// Handle a client side streaming gRPC request.
    pub async fn client_streaming<S, B>(
        &mut self,
        mut service: S,
        req: http::Request<B>,
    ) -> http::Response<BoxBody>
    where
        S: ClientStreamingService<T::Decode, Response = T::Encode>,
        B: Body + Send + Sync + 'static,
        B::Error: Into<crate::Error> + Send + 'static,
    {
        // TODO(david): encoding

        let request = self.map_request_streaming(req);
        let response = service
            .call(request)
            .await
            .map(|r| r.map(|m| stream::once(future::ok(m))));
        self.map_response(response, None)
    }

    /// Handle a bi-directional streaming gRPC request.
    pub async fn streaming<S, B>(
        &mut self,
        mut service: S,
        req: http::Request<B>,
    ) -> http::Response<BoxBody>
    where
        S: StreamingService<T::Decode, Response = T::Encode> + Send,
        S::ResponseStream: Send + Sync + 'static,
        B: Body + Send + Sync + 'static,
        B::Error: Into<crate::Error> + Send,
    {
        // TODO(david): encoding
        let request = self.map_request_streaming(req);
        let response = service.call(request).await;
        self.map_response(response, None)
    }

    async fn map_request_unary<B>(
        &mut self,
        request: http::Request<B>,
    ) -> Result<Request<T::Decode>, Status>
    where
        B: Body + Send + Sync + 'static,
        B::Error: Into<crate::Error> + Send,
    {
        // TODO(david): probably should set this directly on `Grpc`, like the client
        let request_compression_encoding = if let Some(request_compression_encoding) =
            CompressionEncoding::from_encoding_header(request.headers())
        {
            let encoding_supported = match request_compression_encoding {
                CompressionEncoding::Gzip => self.accept_compression_encodings.gzip(),
            };

            if encoding_supported {
                Some(request_compression_encoding)
            } else {
                return Err(Status::unimplemented(format!(
                    "Request is compressed with `{}` which the server doesn't support",
                    request_compression_encoding
                )));
            }
        } else {
            None
        };

        let (parts, body) = request.into_parts();
        let stream =
            Streaming::new_request(self.codec.decoder(), body, request_compression_encoding);

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
        B: Body + Send + Sync + 'static,
        B::Error: Into<crate::Error> + Send,
    {
        Request::from_http(request.map(|body| {
            // TODO(david): get compression encoding from request and don't hard code `None`
            Streaming::new_request(self.codec.decoder(), body, None)
        }))
    }

    fn map_response<B>(
        &mut self,
        response: Result<crate::Response<B>, Status>,
        encoding: Option<CompressionEncoding>,
    ) -> http::Response<BoxBody>
    where
        B: TryStream<Ok = T::Encode, Error = Status> + Send + Sync + 'static,
    {
        let response = match response {
            Ok(r) => r,
            Err(status) => return status.to_http(),
        };

        let (mut parts, body) = response.into_http().into_parts();

        // Set the content type
        parts.headers.insert(
            http::header::CONTENT_TYPE,
            http::header::HeaderValue::from_static("application/grpc"),
        );

        if let Some(encoding) = encoding {
            // Set the content encoding
            parts.headers.insert(
                crate::codec::compression::ENCODING_HEADER,
                encoding.into_header_value(),
            );
        }

        let body = encode_server(self.codec.encoder(), body.into_stream(), encoding);

        http::Response::from_parts(parts, BoxBody::new(body))
    }
}

impl<T: fmt::Debug> fmt::Debug for Grpc<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Grpc").field("codec", &self.codec).finish()
    }
}
