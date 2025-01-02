use tokio_stream::StreamExt;
use tonic::{IntoStreamingRequest, Response, Status};

tonic::include_proto!("test");

#[derive(Debug, Default)]
pub struct Svc;

#[tonic::async_trait]
impl test_server::Test for Svc {
    async fn test_request(
        &self,
        req: impl IntoStreamingRequest<Message = Result<Message, Status>, Stream: Unpin> + Send,
    ) -> Result<Response<Message>, Status> {
        let mut req = req.into_streaming_request().into_inner();
        while let Some(message) = req.try_next().await? {
            println!("Got message: {message:?}")
        }

        Ok(Response::new(Message {}))
    }
}

#[cfg(test)]
mod tests {
    use super::test_server::Test;
    use super::*;

    #[tokio::test]
    async fn test_request_handler() {
        let incoming_messages = tokio_stream::iter([Message {}, Message {}].map(Ok));
        let svc = Svc;
        svc.test_request(incoming_messages).await.unwrap();
    }
}
