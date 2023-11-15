use super::*;
use bytes::Bytes;
use http_body::Body;
use pin_project::pin_project;
use std::{
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering::SeqCst},
        Arc,
    },
    task::{ready, Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tonic::codec::CompressionEncoding;
use tonic::transport::{server::Connected, Channel};
use tower_http::map_request_body::MapRequestBodyLayer;

macro_rules! parametrized_tests {
    ($fn_name:ident, $($test_name:ident: $input:expr),+ $(,)?) => {
        paste::paste! {
            $(
                #[tokio::test(flavor = "multi_thread")]
                async fn [<$fn_name _ $test_name>]() {
                    let input = $input;
                    $fn_name(input).await;
                }
            )+
        }
    }
}

pub(crate) use parametrized_tests;

/// A body that tracks how many bytes passes through it
#[pin_project]
pub struct CountBytesBody<B> {
    #[pin]
    pub inner: B,
    pub counter: Arc<AtomicUsize>,
}

impl<B> Body for CountBytesBody<B>
where
    B: Body<Data = Bytes>,
{
    type Data = B::Data;
    type Error = B::Error;

    fn poll_data(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        let this = self.project();
        let counter: Arc<AtomicUsize> = this.counter.clone();
        match ready!(this.inner.poll_data(cx)) {
            Some(Ok(chunk)) => {
                println!("response body chunk size = {}", chunk.len());
                counter.fetch_add(chunk.len(), SeqCst);
                Poll::Ready(Some(Ok(chunk)))
            }
            x => Poll::Ready(x),
        }
    }

    fn poll_trailers(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<http::HeaderMap>, Self::Error>> {
        self.project().inner.poll_trailers(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

#[allow(dead_code)]
pub fn measure_request_body_size_layer(
    bytes_sent_counter: Arc<AtomicUsize>,
) -> MapRequestBodyLayer<impl Fn(hyper::Body) -> hyper::Body + Clone> {
    MapRequestBodyLayer::new(move |mut body: hyper::Body| {
        let (mut tx, new_body) = hyper::Body::channel();

        let bytes_sent_counter = bytes_sent_counter.clone();
        tokio::spawn(async move {
            while let Some(chunk) = body.data().await {
                let chunk = chunk.unwrap();
                println!("request body chunk size = {}", chunk.len());
                bytes_sent_counter.fetch_add(chunk.len(), SeqCst);
                tx.send_data(chunk).await.unwrap();
            }

            if let Some(trailers) = body.trailers().await.unwrap() {
                tx.send_trailers(trailers).await.unwrap();
            }
        });

        new_body
    })
}

#[allow(dead_code)]
pub async fn mock_io_channel(client: tokio::io::DuplexStream) -> Channel {
    let mut client = Some(client);

    Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector(service_fn(move |_: Uri| {
            let client = client.take().unwrap();
            async move { Ok::<_, std::io::Error>(client) }
        }))
        .await
        .unwrap()
}

#[derive(Clone)]
pub struct AssertRightEncoding {
    encoding: CompressionEncoding,
}

#[allow(dead_code)]
impl AssertRightEncoding {
    pub fn new(encoding: CompressionEncoding) -> Self {
        Self { encoding }
    }

    pub fn call<B: Body>(self, req: http::Request<B>) -> http::Request<B> {
        let expected = match self.encoding {
            CompressionEncoding::Gzip => "gzip",
            CompressionEncoding::Zstd => "zstd",
            _ => panic!("unexpected encoding {:?}", self.encoding),
        };
        assert_eq!(req.headers().get("grpc-encoding").unwrap(), expected);

        req
    }
}
