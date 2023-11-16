use crate::arrow;
use tonic::codegen::{Body, StdError};

pub enum FlightClient<T> {
    Manual(arrow::manual::flight_service_client::FlightServiceClient<T>),
    Native(arrow::flight_service_client::FlightServiceClient<T>),
}

impl<T> FlightClient<T>
where
    T: tonic::client::GrpcService<tonic::body::BoxBody>,
    T::Error: Into<StdError>,
    T::ResponseBody: Body + Send + 'static,
    <T::ResponseBody as Body>::Data: Into<tonic::codec::SliceBuffer> + Send,
    <T::ResponseBody as Body>::Error: Into<StdError> + Send,
{
    pub fn manual(inner: T) -> Self {
        FlightClient::Manual(arrow::manual::flight_service_client::FlightServiceClient::new(inner))
    }

    pub fn native(inner: T) -> Self {
        FlightClient::Native(arrow::flight_service_client::FlightServiceClient::new(
            inner,
        ))
    }

    pub async fn do_exchange(
        &mut self,
        request: impl tonic::IntoStreamingRequest<Message = arrow::FlightData>,
    ) -> Result<tonic::Response<tonic::codec::Streaming<arrow::FlightData>>, tonic::Status> {
        match self {
            FlightClient::Manual(client) => client.do_exchange(request).await,
            FlightClient::Native(client) => client.do_exchange(request).await,
        }
    }
}
