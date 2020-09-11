use std::pin::Pin;
use tokio::stream::{self, Stream, StreamExt};
use tonic::{Request, Response, Status, Streaming};

use crate::pb::{self, test_service_server::TestService};

type BoxStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send + Sync + 'static>>;

pub struct Test;

#[tonic::async_trait]
impl TestService for Test {
    type StreamingOutputCallStream = BoxStream<pb::StreamingOutputCallResponse>;
    type FullDuplexCallStream = BoxStream<pb::StreamingOutputCallResponse>;
    type HalfDuplexCallStream = BoxStream<pb::StreamingOutputCallResponse>;

    async fn empty_call(&self, _req: Request<pb::Empty>) -> Result<Response<pb::Empty>, Status> {
        Ok(Response::new(pb::Empty {}))
    }

    async fn unary_call(
        &self,
        req: Request<pb::SimpleRequest>,
    ) -> Result<Response<pb::SimpleResponse>, Status> {
        if let Some(ref response_status) = req.get_ref().response_status {
            return Err(Status::new(
                tonic::Code::from_i32(response_status.code),
                response_status.message.to_owned(),
            ));
        }

        let size = req.into_inner().response_size as usize;

        let mut payload = pb::Payload::default();
        payload.body = vec![0; size];

        let mut res = pb::SimpleResponse::default();
        res.payload = Some(payload);

        Ok(Response::new(res))
    }

    async fn cacheable_unary_call(
        &self,
        _req: Request<pb::SimpleRequest>,
    ) -> Result<Response<pb::SimpleResponse>, Status> {
        unimplemented!()
    }

    async fn streaming_output_call(
        &self,
        req: Request<pb::StreamingOutputCallRequest>,
    ) -> Result<Response<Self::StreamingOutputCallStream>, Status> {
        let stream = Box::pin(
            stream::iter(req.into_inner().response_parameters).map(|param| {
                Ok::<_, Status>(pb::StreamingOutputCallResponse {
                    payload: Some(pb::Payload {
                        body: vec![0; param.size as usize],
                        ..Default::default()
                    }),
                })
            }),
        );

        Ok(Response::new(stream))
    }

    async fn streaming_input_call(
        &self,
        _req: Request<Streaming<pb::StreamingInputCallRequest>>,
    ) -> Result<Response<pb::StreamingInputCallResponse>, Status> {
        unimplemented!()
    }

    async fn full_duplex_call(
        &self,
        _req: Request<Streaming<pb::StreamingOutputCallRequest>>,
    ) -> Result<Response<Self::FullDuplexCallStream>, Status> {
        unimplemented!()
    }

    async fn half_duplex_call(
        &self,
        _req: Request<Streaming<pb::StreamingOutputCallRequest>>,
    ) -> Result<Response<Self::HalfDuplexCallStream>, Status> {
        unimplemented!()
    }

    async fn unimplemented_call(
        &self,
        _req: Request<pb::Empty>,
    ) -> Result<Response<pb::Empty>, Status> {
        Err(Status::unimplemented("unimplemented call"))
    }
}
