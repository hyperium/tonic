use pb::{test_server::Test, Input, Output};
use std::pin::Pin;
use tokio::stream::{self, Stream, StreamExt};
use tonic::{Request, Response, Status, Streaming};

pub mod pb {
    tonic::include_proto!("test");
}

type BoxStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send + Sync + 'static>>;

pub struct Svc;

#[tonic::async_trait]
impl Test for Svc {
    async fn unary_call(&self, req: Request<Input>) -> Result<Response<Output>, Status> {
        let req = req.into_inner();

        if &req.desc == "boom" {
            Err(Status::invalid_argument("invalid boom"))
        } else {
            Ok(Response::new(Output {
                id: req.id,
                desc: req.desc,
            }))
        }
    }

    type ServerStreamStream = BoxStream<Output>;

    async fn server_stream(
        &self,
        req: Request<Input>,
    ) -> Result<Response<Self::ServerStreamStream>, Status> {
        let req = req.into_inner();

        Ok(Response::new(Box::pin(stream::iter(vec![1, 2]).map(
            move |n| {
                Ok(Output {
                    id: req.id,
                    desc: format!("{}-{}", n, req.desc),
                })
            },
        ))))
    }

    async fn client_stream(
        &self,
        req: Request<Streaming<Input>>,
    ) -> Result<Response<Output>, Status> {
        let out = Output {
            id: 0,
            desc: "".into(),
        };

        Ok(Response::new(
            req.into_inner()
                .fold(out, |mut acc, input| {
                    let input = input.unwrap();
                    acc.id += input.id;
                    acc.desc += &input.desc;
                    acc
                })
                .await,
        ))
    }
}
