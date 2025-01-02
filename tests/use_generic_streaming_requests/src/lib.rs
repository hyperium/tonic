use tokio_stream::StreamExt;
use tonic::{Response, Status};

tonic::include_proto!("test");

#[derive(Debug, Default)]
pub struct Svc;

#[tonic::async_trait]
impl test_server::Test for Svc {
    async fn test_request(
        &self,
        req: tonic::Request<
            impl tokio_stream::Stream<Item = Result<Message, Status>> + Send + Unpin,
        >,
    ) -> Result<Response<Message>, Status> {
        let mut req = req.into_inner();
        while let Some(message) = req.try_next().await? {
            println!("Got message: {message:?}")
        }

        Ok(Response::new(Message {}))
    }
}

#[cfg(test)]
mod tests {
    use tonic::Request;

    use super::test_server::Test;
    use super::*;

    #[tokio::test]
    async fn test_request_handler() {
        let incoming_messages = tokio_stream::iter([Message {}, Message {}].map(Ok));
        let svc = Svc;
        svc.test_request(Request::new(incoming_messages))
            .await
            .unwrap();
    }
}
