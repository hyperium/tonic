use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use http::Uri;
use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;
use tower::Service;

use crate::status::ConnectError;

pub(crate) struct UdsConnector {
    uds_filepath: String,
}

impl UdsConnector {
    pub(crate) fn new(uds_filepath: &str) -> Self {
        UdsConnector {
            uds_filepath: uds_filepath.to_string(),
        }
    }
}

impl Service<Uri> for UdsConnector {
    type Response = TokioIo<UnixStream>;
    type Error = ConnectError;
    type Future = UdsConnecting;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: Uri) -> Self::Future {
        let uds_path = self.uds_filepath.clone();
        let fut = async move {
            let stream = UnixStream::connect(uds_path)
                .await
                .map_err(|err| ConnectError(From::from(err)))?;
            Ok(TokioIo::new(stream))
        };
        UdsConnecting {
            inner: Box::pin(fut),
        }
    }
}

type ConnectResult = Result<TokioIo<UnixStream>, ConnectError>;

pub(crate) struct UdsConnecting {
    inner: Pin<Box<dyn Future<Output = ConnectResult> + Send>>,
}

impl Future for UdsConnecting {
    type Output = ConnectResult;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().inner.as_mut().poll(cx)
    }
}
