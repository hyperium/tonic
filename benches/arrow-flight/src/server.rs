use crate::arrow;
use tokio::sync::mpsc;
use tonic::{
    codegen::{
        tokio_stream::{wrappers::ReceiverStream, StreamExt},
        BoxStream,
    },
    Request, Response, Status, Streaming,
};

pub struct FlightService {
    payload: arrow::FlightData,
}

impl FlightService {
    pub fn new(payload: arrow::FlightData) -> Self {
        FlightService { payload }
    }

    async fn exchange(
        &self,
        request: Request<Streaming<arrow::FlightData>>,
    ) -> Result<Response<BoxStream<arrow::FlightData>>, Status> {
        let mut stream = request.into_inner();
        let payload = self.payload.clone();
        let (tx, rx) = mpsc::channel(8192);
        tokio::spawn(async move {
            while let Some(_data) = stream.next().await.transpose().unwrap() {
                tx.send(Ok(payload.clone())).await.unwrap();
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }
}

#[tonic::async_trait]
impl arrow::flight_service_server::FlightService for FlightService {
    async fn do_exchange(
        &self,
        request: Request<Streaming<arrow::FlightData>>,
    ) -> Result<Response<BoxStream<arrow::FlightData>>, Status> {
        self.exchange(request).await
    }
}

#[tonic::async_trait]
impl arrow::manual::flight_service_server::FlightService for FlightService {
    type DoExchangeStream = BoxStream<arrow::FlightData>;

    async fn do_exchange(
        &self,
        request: Request<Streaming<arrow::FlightData>>,
    ) -> Result<Response<Self::DoExchangeStream>, Status> {
        self.exchange(request).await
    }
}
