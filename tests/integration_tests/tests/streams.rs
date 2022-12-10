use futures::FutureExt;
use integration_tests::pb::{test_stream_server, InputStream, OutputStream};
use tonic::{transport::Server, Request, Response, Status};

type Stream<T> =
    std::pin::Pin<Box<dyn futures::Stream<Item = std::result::Result<T, Status>> + Send + 'static>>;

#[tokio::test]
async fn status_from_server_stream_with_source() {
    struct Svc;

    #[tonic::async_trait]
    impl test_stream_server::TestStream for Svc {
        type StreamCallStream = Stream<OutputStream>;

        async fn stream_call(
            &self,
            _: Request<InputStream>,
        ) -> Result<Response<Self::StreamCallStream>, Status> {
            let s = Unsync(std::ptr::null_mut::<()>());

            Ok(Response::new(Box::pin(s) as Self::StreamCallStream))
        }
    }

    let svc = test_stream_server::TestStreamServer::new(Svc);

    Server::builder()
        .add_service(svc)
        .serve("127.0.0.1:1339".parse().unwrap())
        .now_or_never();
}

struct Unsync(*mut ());

unsafe impl Send for Unsync {}

impl futures::Stream for Unsync {
    type Item = Result<OutputStream, Status>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        unimplemented!()
    }
}
