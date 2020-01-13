use crate::{
    body::{Body, BoxBody},
    client::GrpcService,
    codec::{encode_client, Codec, Streaming},
    interceptor::Interceptor,
    Code, Request, Response, Status,
};
use futures_core::Stream;
use futures_util::{future, stream, TryStreamExt};
use http::{
    header::{HeaderValue, CONTENT_TYPE, TE},
    uri::{Parts, PathAndQuery, Uri},
};
use http_body::Body as HttpBody;
use std::fmt;

/// A gRPC client dispatcher.
///
/// This will wrap some inner [`GrpcService`] and will encode/decode
/// messages via the provided codec.
///
/// Each request method takes a [`Request`], a [`PathAndQuery`], and a
/// [`Codec`]. The request contains the message to send via the
/// [`Codec::encoder`]. The path determines the fully qualified path
/// that will be append to the outgoing uri. The path must follow
/// the conventions explained in the [gRPC protocol definition] under `Path →`. An
/// example of this path could look like `/greeter.Greeter/SayHello`.
///
/// [gRPC protocol definition]: https://github.com/grpc/grpc/blob/master/doc/PROTOCOL-HTTP2.md#requests
pub struct Grpc<T> {
    inner: T,
    interceptor: Option<Interceptor>,
}

impl<T> Grpc<T> {
    /// Creates a new gRPC client with the provided [`GrpcService`].
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            interceptor: None,
        }
    }

    /// Creates a new gRPC client with the provided [`GrpcService`] and will apply
    /// the provided interceptor on each request.
    pub fn with_interceptor(inner: T, interceptor: impl Into<Interceptor>) -> Self {
        Self {
            inner,
            interceptor: Some(interceptor.into()),
        }
    }

    /// Check if the inner [`GrpcService`] is able to accept a  new request.
    ///
    /// This will call [`GrpcService::poll_ready`] until it returns ready or
    /// an error. If this returns ready the inner [`GrpcService`] is ready to
    /// accept one more request.
    pub async fn ready(&mut self) -> Result<(), T::Error>
    where
        T: GrpcService<BoxBody>,
    {
        future::poll_fn(|cx| self.inner.poll_ready(cx)).await
    }

    /// Send a single unary gRPC request.
    pub async fn unary<M1, M2, C>(
        &mut self,
        request: Request<M1>,
        path: PathAndQuery,
        codec: C,
    ) -> Result<Response<M2>, Status>
    where
        T: GrpcService<BoxBody>,
        T::ResponseBody: Body + HttpBody + Send + 'static,
        <T::ResponseBody as HttpBody>::Error: Into<crate::Error>,
        C: Codec<Encode = M1, Decode = M2>,
        M1: Send + Sync + 'static,
        M2: Send + Sync + 'static,
    {
        let request = request.map(|m| stream::once(future::ready(m)));
        self.client_streaming(request, path, codec).await
    }

    /// Send a client side streaming gRPC request.
    pub async fn client_streaming<S, M1, M2, C>(
        &mut self,
        request: Request<S>,
        path: PathAndQuery,
        codec: C,
    ) -> Result<Response<M2>, Status>
    where
        T: GrpcService<BoxBody>,
        T::ResponseBody: Body + HttpBody + Send + 'static,
        <T::ResponseBody as HttpBody>::Error: Into<crate::Error>,
        S: Stream<Item = M1> + Send + Sync + 'static,
        C: Codec<Encode = M1, Decode = M2>,
        M1: Send + Sync + 'static,
        M2: Send + Sync + 'static,
    {
        let (mut parts, body) = self.streaming(request, path, codec).await?.into_parts();

        futures_util::pin_mut!(body);

        let message = body
            .try_next()
            .await?
            .ok_or_else(|| Status::new(Code::Internal, "Missing response message."))?;

        if let Some(trailers) = body.trailers().await? {
            parts.merge(trailers);
        }

        Ok(Response::from_parts(parts, message))
    }

    /// Send a server side streaming gRPC request.
    pub async fn server_streaming<M1, M2, C>(
        &mut self,
        request: Request<M1>,
        path: PathAndQuery,
        codec: C,
    ) -> Result<Response<Streaming<M2>>, Status>
    where
        T: GrpcService<BoxBody>,
        T::ResponseBody: Body + HttpBody + Send + 'static,
        <T::ResponseBody as HttpBody>::Error: Into<crate::Error>,
        C: Codec<Encode = M1, Decode = M2>,
        M1: Send + Sync + 'static,
        M2: Send + Sync + 'static,
    {
        let request = request.map(|m| stream::once(future::ready(m)));
        self.streaming(request, path, codec).await
    }

    /// Send a bi-directional streaming gRPC request.
    pub async fn streaming<S, M1, M2, C>(
        &mut self,
        request: Request<S>,
        path: PathAndQuery,
        mut codec: C,
    ) -> Result<Response<Streaming<M2>>, Status>
    where
        T: GrpcService<BoxBody>,
        T::ResponseBody: Body + HttpBody + Send + 'static,
        <T::ResponseBody as HttpBody>::Error: Into<crate::Error>,
        S: Stream<Item = M1> + Send + Sync + 'static,
        C: Codec<Encode = M1, Decode = M2>,
        M1: Send + Sync + 'static,
        M2: Send + Sync + 'static,
    {
        let request = if let Some(interceptor) = &self.interceptor {
            interceptor.call(request)?
        } else {
            request
        };

        let mut parts = Parts::default();
        parts.path_and_query = Some(path);

        let uri = Uri::from_parts(parts).expect("path_and_query only is valid Uri");

        let request = request
            .map(|s| encode_client(codec.encoder(), s))
            .map(BoxBody::new);

        let mut request = request.into_http(uri);

        // Add the gRPC related HTTP headers
        request
            .headers_mut()
            .insert(TE, HeaderValue::from_static("trailers"));

        // Set the content type
        request
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static("application/grpc"));

        let response = self
            .inner
            .call(request)
            .await
            .map_err(|err| Status::from_error(&*(err.into())))?;

        let status_code = response.status();
        let trailers_only_status = Status::from_header_map(response.headers());

        // We do not need to check for trailers if the `grpc-status` header is present
        // with a valid code.
        let expect_additional_trailers = if let Some(status) = trailers_only_status {
            if status.code() != Code::Ok {
                return Err(status);
            }

            false
        } else {
            true
        };

        let response = response.map(|body| {
            if expect_additional_trailers {
                Streaming::new_response(codec.decoder(), body, status_code)
            } else {
                Streaming::new_empty(codec.decoder(), body)
            }
        });

        Ok(Response::from_http(response))
    }
}

impl<T: Clone> Clone for Grpc<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            interceptor: self.interceptor.clone(),
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for Grpc<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Grpc").field("inner", &self.inner).finish()
    }
}
