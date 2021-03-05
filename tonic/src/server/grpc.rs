use crate::{
    body::BoxBody,
    codec::{encode_server, Codec, Streaming},
    interceptor::Interceptor,
    reporter::{Reporter, ReporterCallback, RpcType},
    server::{
        ClientStreamingService, NamedMethod, ServerStreamingService, StreamingService, UnaryService,
    },
    Code, Request, Status,
};
use futures_core::TryStream;
use futures_util::{future, stream, TryStreamExt};
use http_body::Body;
use std::fmt;
use std::sync::Arc;

// A try! type macro for intercepting requests
macro_rules! t {
    ($expr : expr) => {
        match $expr {
            Ok(request) => request,
            Err(res) => return res,
        }
    };
}

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
    interceptor: Option<Interceptor>,
    reporter: Option<Reporter>,
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
            interceptor: None,
            reporter: None,
        }
    }

    /// Creates a new gRPC server with the provided [`Codec`] and will apply the provided
    /// interceptor on each inbound request.
    pub fn with_interceptor(codec: T, interceptor: impl Into<Interceptor>) -> Self {
        Self {
            codec,
            interceptor: Some(interceptor.into()),
            reporter: None,
        }
    }

    /// Creates a new gRPC server with the provided [`Codec`] and will apply the provided
    /// reporter to all requests.
    pub fn with_reporter(codec: T, reporter: impl Into<Reporter>) -> Self {
        Self {
            codec,
            interceptor: None,
            reporter: Some(reporter.into()),
        }
    }

    /// Creates a new gRPC server with the provided [`Codec`] and will apply the provided
    /// interceptor on each inbound request and reporter to all requests.
    pub fn with_interceptor_reporter(
        codec: T,
        interceptor: impl Into<Interceptor>,
        reporter: impl Into<Reporter>,
    ) -> Self {
        Self {
            codec,
            interceptor: Some(interceptor.into()),
            reporter: Some(reporter.into()),
        }
    }

    /// Handle a single unary gRPC request.
    pub async fn unary<S, B>(
        &mut self,
        mut service: S,
        req: http::Request<B>,
    ) -> http::Response<BoxBody>
    where
        S: UnaryService<T::Decode, Response = T::Encode> + NamedMethod,
        B: Body + Send + Sync + 'static,
        B::Error: Into<crate::Error> + Send,
    {
        let reporter_callback = self.report_rpc_start::<S>(RpcType::Unary);

        let request = match self
            .map_request_unary(req, reporter_callback.as_ref().cloned())
            .await
        {
            Ok(r) => r,
            Err(status) => {
                return self
                    .map_response::<stream::Once<future::Ready<Result<T::Encode, Status>>>>(
                        Err(status),
                        reporter_callback,
                    );
            }
        };

        let request = t!(self.intercept_request(request));

        let response = service
            .call(request)
            .await
            .map(|r| r.map(|m| stream::once(future::ok(m))));

        self.map_response(response, reporter_callback)
    }

    /// Handle a server side streaming request.
    pub async fn server_streaming<S, B>(
        &mut self,
        mut service: S,
        req: http::Request<B>,
    ) -> http::Response<BoxBody>
    where
        S: ServerStreamingService<T::Decode, Response = T::Encode> + NamedMethod,
        S::ResponseStream: Send + Sync + 'static,
        B: Body + Send + Sync + 'static,
        B::Error: Into<crate::Error> + Send,
    {
        let reporter_callback = self.report_rpc_start::<S>(RpcType::ServerStreaming);

        let request = match self
            .map_request_unary(req, reporter_callback.as_ref().cloned())
            .await
        {
            Ok(r) => r,
            Err(status) => {
                return self.map_response::<S::ResponseStream>(Err(status), reporter_callback);
            }
        };

        let request = t!(self.intercept_request(request));

        let response = service.call(request).await;

        self.map_response(response, reporter_callback)
    }

    /// Handle a client side streaming gRPC request.
    pub async fn client_streaming<S, B>(
        &mut self,
        mut service: S,
        req: http::Request<B>,
    ) -> http::Response<BoxBody>
    where
        S: ClientStreamingService<T::Decode, Response = T::Encode> + NamedMethod,
        B: Body + Send + Sync + 'static,
        B::Error: Into<crate::Error> + Send + 'static,
    {
        let reporter_callback = self.report_rpc_start::<S>(RpcType::ClientStreaming);
        let request = self.map_request_streaming(req, reporter_callback.as_ref().cloned());
        let request = t!(self.intercept_request(request));
        let response = service
            .call(request)
            .await
            .map(|r| r.map(|m| stream::once(future::ok(m))));
        self.map_response(response, reporter_callback)
    }

    /// Handle a bi-directional streaming gRPC request.
    pub async fn streaming<S, B>(
        &mut self,
        mut service: S,
        req: http::Request<B>,
    ) -> http::Response<BoxBody>
    where
        S: StreamingService<T::Decode, Response = T::Encode> + NamedMethod + Send,
        S::ResponseStream: Send + Sync + 'static,
        B: Body + Send + Sync + 'static,
        B::Error: Into<crate::Error> + Send,
    {
        let reporter_callback = self.report_rpc_start::<S>(RpcType::Streaming);
        let request = self.map_request_streaming(req, reporter_callback.as_ref().cloned());
        let request = t!(self.intercept_request(request));
        let response = service.call(request).await;
        self.map_response(response, reporter_callback)
    }

    async fn map_request_unary<B>(
        &mut self,
        request: http::Request<B>,
        reporter_callback: Option<Arc<dyn ReporterCallback + Send + Sync + 'static>>,
    ) -> Result<Request<T::Decode>, Status>
    where
        B: Body + Send + Sync + 'static,
        B::Error: Into<crate::Error> + Send,
    {
        let (parts, body) = request.into_parts();
        let stream = Streaming::new_request(self.codec.decoder(), body, reporter_callback);

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
        reporter_callback: Option<Arc<dyn ReporterCallback + Send + Sync + 'static>>,
    ) -> Request<Streaming<T::Decode>>
    where
        B: Body + Send + Sync + 'static,
        B::Error: Into<crate::Error> + Send,
    {
        Request::from_http(
            request
                .map(|body| Streaming::new_request(self.codec.decoder(), body, reporter_callback)),
        )
    }

    fn map_response<B>(
        &mut self,
        response: Result<crate::Response<B>, Status>,
        reporter_callback: Option<Arc<dyn ReporterCallback + Send + Sync + 'static>>,
    ) -> http::Response<BoxBody>
    where
        B: TryStream<Ok = T::Encode, Error = Status> + Send + Sync + 'static,
    {
        match response {
            Ok(r) => {
                self.report_rpc_complete(reporter_callback.clone(), &Status::ok(""));

                let (mut parts, body) = r.into_http().into_parts();

                // Set the content type
                parts.headers.insert(
                    http::header::CONTENT_TYPE,
                    http::header::HeaderValue::from_static("application/grpc"),
                );

                let callback = reporter_callback.clone();
                let body = body.into_stream().inspect_ok(move |_| {
                    if let Some(ref r) = callback {
                        r.stream_message_sent();
                    }
                });
                let body = encode_server(self.codec.encoder(), body);

                http::Response::from_parts(parts, BoxBody::new(body))
            }
            Err(status) => {
                self.report_rpc_complete(reporter_callback, &status);
                status.to_http()
            }
        }
    }

    fn intercept_request<A>(&self, req: Request<A>) -> Result<Request<A>, http::Response<BoxBody>> {
        if let Some(interceptor) = &self.interceptor {
            match interceptor.call(req) {
                Ok(req) => Ok(req),
                Err(status) => Err(status.to_http()),
            }
        } else {
            Ok(req)
        }
    }

    fn report_rpc_start<M: NamedMethod>(
        &self,
        rpc_type: RpcType,
    ) -> Option<Arc<dyn ReporterCallback + Send + Sync + 'static>> {
        self.reporter.as_ref().map(|r| {
            let callback = r.report_rpc_start(
                <M as NamedMethod>::SERVICE_NAME,
                <M as NamedMethod>::METHOD_NAME,
                rpc_type,
            );
            let callback: Arc<dyn ReporterCallback + Send + Sync + 'static> = Arc::from(callback);
            callback
        })
    }

    fn report_rpc_complete(
        &self,
        reporter_guard: Option<Arc<dyn ReporterCallback + Send + Sync + 'static>>,
        status: &Status,
    ) {
        if let Some(r) = reporter_guard {
            r.rpc_complete(status.clone());
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for Grpc<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Grpc").field("codec", &self.codec).finish()
    }
}
