use crate::grpc_pb::{self, *};
use async_stream::try_stream;
use http::header::{HeaderMap, HeaderName};
use http_body_util::BodyExt;
use std::future::Future;
use std::pin::Pin;
use std::result::Result as StdResult;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio_stream::StreamExt;
use tonic::codegen::BoxStream;
use tonic::{body::Body, server::NamedService, Code, Request, Response, Status};
use tonic_protobuf::protobuf::proto;
use tower::Service;

pub use grpc_pb::test_service_server::TestServiceServer;
pub use grpc_pb::unimplemented_service_server::UnimplementedServiceServer;

#[derive(Default, Clone)]
pub struct TestService {}

type Result<T> = StdResult<Response<T>, Status>;
type Streaming<T> = Request<tonic::Streaming<T>>;
type BoxFuture<T, E> = Pin<Box<dyn Future<Output = StdResult<T, E>> + Send + 'static>>;

#[tonic::async_trait]
impl grpc_pb::test_service_server::TestService for TestService {
    async fn empty_call(&self, _request: Request<Empty>) -> Result<Empty> {
        Ok(Response::new(Empty::default()))
    }

    async fn unary_call(&self, request: Request<SimpleRequest>) -> Result<SimpleResponse> {
        let req = request.into_inner();

        if req.response_status().code() != 0 {
            let echo_status = req.response_status();
            let status = Status::new(
                Code::from_i32(echo_status.code()),
                echo_status.message().to_string(),
            );
            return Err(status);
        }

        let res_size = if req.response_size() >= 0 {
            req.response_size() as usize
        } else {
            let status = Status::new(Code::InvalidArgument, "response_size cannot be negative");
            return Err(status);
        };

        let res = proto!(SimpleResponse {
            payload: Payload {
                body: vec![0; res_size],
            },
        });

        Ok(Response::new(res))
    }

    async fn cacheable_unary_call(&self, _: Request<SimpleRequest>) -> Result<SimpleResponse> {
        unimplemented!()
    }

    async fn streaming_output_call(
        &self,
        req: tonic::Request<StreamingOutputCallRequest>,
    ) -> std::result::Result<tonic::Response<BoxStream<StreamingOutputCallResponse>>, tonic::Status>
    {
        let stream = try_stream! {
            for param in req.into_inner().response_parameters() {
                tokio::time::sleep(Duration::from_micros(param.interval_us() as u64)).await;

                let payload = crate::grpc_utils::server_payload(param.size() as usize);
                yield proto!(StreamingOutputCallResponse { payload: payload });
            }
        };

        Ok(Response::new(Box::pin(stream)))
    }

    async fn streaming_input_call(
        &self,
        req: Streaming<StreamingInputCallRequest>,
    ) -> Result<StreamingInputCallResponse> {
        let mut stream = req.into_inner();

        let mut aggregated_payload_size = 0;
        while let Some(msg) = stream.try_next().await? {
            aggregated_payload_size += msg.payload().body().len() as i32;
        }

        let res = proto!(StreamingInputCallResponse {
            aggregated_payload_size: aggregated_payload_size,
        });

        Ok(Response::new(res))
    }

    async fn full_duplex_call(
        &self,
        req: tonic::Request<tonic::Streaming<StreamingOutputCallRequest>>,
    ) -> std::result::Result<tonic::Response<BoxStream<StreamingOutputCallResponse>>, tonic::Status>
    {
        let mut stream = req.into_inner();

        if let Some(first_msg) = stream.message().await? {
            if first_msg.response_status().code() != 0 {
                let echo_status = first_msg.response_status();
                let status = Status::new(
                    Code::from_i32(echo_status.code()),
                    echo_status.message().to_string(),
                );
                return Err(status);
            }

            let single_message = tokio_stream::once(Ok(first_msg));
            let mut stream = single_message.chain(stream);

            let stream = try_stream! {
                while let Some(msg) = stream.try_next().await? {
                    if msg.response_status().code() != 0 {
                        let echo_status = msg.response_status();
                        let status = Status::new(Code::from_i32(echo_status.code()), echo_status.message().to_string());
                        Err(status)?;
                    }

                    for param in msg.response_parameters() {
                        tokio::time::sleep(Duration::from_micros(param.interval_us() as u64)).await;

                        let payload = crate::grpc_utils::server_payload(param.size() as usize);
                        yield proto!(StreamingOutputCallResponse { payload: payload });
                    }
                }
            };

            Ok(Response::new(Box::pin(stream)))
        } else {
            let stream = tokio_stream::empty();
            Ok(Response::new(Box::pin(stream)))
        }
    }

    async fn half_duplex_call(
        &self,
        _request: tonic::Request<tonic::Streaming<StreamingOutputCallRequest>>,
    ) -> std::result::Result<tonic::Response<BoxStream<StreamingOutputCallResponse>>, tonic::Status>
    {
        Err(Status::unimplemented("TODO"))
    }

    async fn unimplemented_call(&self, _: Request<Empty>) -> Result<Empty> {
        Err(Status::unimplemented(""))
    }
}

#[derive(Default)]
pub struct UnimplementedService {}

#[tonic::async_trait]
impl grpc_pb::unimplemented_service_server::UnimplementedService for UnimplementedService {
    async fn unimplemented_call(&self, _req: Request<Empty>) -> Result<Empty> {
        Err(Status::unimplemented(""))
    }
}

#[derive(Clone, Default)]
pub struct EchoHeadersSvc<S> {
    inner: S,
}

impl<S: NamedService> NamedService for EchoHeadersSvc<S> {
    const NAME: &'static str = S::NAME;
}

impl<S> EchoHeadersSvc<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S> Service<http::Request<Body>> for EchoHeadersSvc<S>
where
    S: Service<http::Request<Body>, Response = http::Response<Body>> + Send,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<StdResult<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, req: http::Request<Body>) -> Self::Future {
        let echo_header = req.headers().get("x-grpc-test-echo-initial").cloned();

        let trailer_name = HeaderName::from_static("x-grpc-test-echo-trailing-bin");
        let echo_trailer = req
            .headers()
            .get(&trailer_name)
            .cloned()
            .map(|v| HeaderMap::from_iter(std::iter::once((trailer_name, v))));

        let call = self.inner.call(req);

        Box::pin(async move {
            let mut res = call.await?;

            if let Some(echo_header) = echo_header {
                res.headers_mut()
                    .insert("x-grpc-test-echo-initial", echo_header);
                Ok(res
                    .map(|b| b.with_trailers(async move { echo_trailer.map(Ok) }))
                    .map(Body::new))
            } else {
                Ok(res)
            }
        })
    }
}
