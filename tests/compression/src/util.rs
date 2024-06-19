use super::*;
use bytes::{Buf, Bytes};
use http_body::{Body, Frame};
use http_body_util::BodyExt as _;
use hyper_util::rt::TokioIo;
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
use tonic::body::BoxBody;
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

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.project();
        let counter: Arc<AtomicUsize> = this.counter.clone();
        match ready!(this.inner.poll_frame(cx)) {
            Some(Ok(chunk)) => {
                println!("response body chunk size = {}", frame_data_length(&chunk));
                counter.fetch_add(frame_data_length(&chunk), SeqCst);
                Poll::Ready(Some(Ok(chunk)))
            }
            x => Poll::Ready(x),
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

fn frame_data_length(frame: &http_body::Frame<Bytes>) -> usize {
    if let Some(data) = frame.data_ref() {
        data.len()
    } else {
        0
    }
}

#[pin_project]
struct ChannelBody<T> {
    #[pin]
    rx: tokio::sync::mpsc::Receiver<Frame<T>>,
}

impl<T> ChannelBody<T> {
    pub fn new() -> (tokio::sync::mpsc::Sender<Frame<T>>, Self) {
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        (tx, Self { rx })
    }
}

impl<T> Body for ChannelBody<T>
where
    T: Buf,
{
    type Data = T;
    type Error = tonic::Status;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let frame = ready!(self.project().rx.poll_recv(cx));
        Poll::Ready(frame.map(Ok))
    }
}

#[allow(dead_code)]
pub fn measure_request_body_size_layer(
    bytes_sent_counter: Arc<AtomicUsize>,
) -> MapRequestBodyLayer<impl Fn(BoxBody) -> BoxBody + Clone> {
    MapRequestBodyLayer::new(move |mut body: BoxBody| {
        let (tx, new_body) = ChannelBody::new();

        let bytes_sent_counter = bytes_sent_counter.clone();
        tokio::spawn(async move {
            while let Some(chunk) = body.frame().await {
                let chunk = chunk.unwrap();
                println!("request body chunk size = {}", frame_data_length(&chunk));
                bytes_sent_counter.fetch_add(frame_data_length(&chunk), SeqCst);
                tx.send(chunk).await.unwrap();
            }
        });

        new_body.boxed_unsync()
    })
}

#[allow(dead_code)]
pub async fn mock_io_channel(client: tokio::io::DuplexStream) -> Channel {
    let mut client = Some(client);

    Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector(service_fn(move |_: Uri| {
            let client = TokioIo::new(client.take().unwrap());
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
