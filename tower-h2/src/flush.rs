use crate::buf::SendBuf;
use futures_util::ready;
use h2::{self, SendStream};
use http::HeaderMap;
use http_body::Body;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Flush a body to the HTTP/2.0 send stream
pub(crate) struct Flush<S>
where
    S: Body,
{
    h2: SendStream<SendBuf<S::Data>>,
    body: S,
    state: FlushState,
}

enum FlushState {
    Data,
    Trailers,
    Done,
}

enum DataOrTrailers<B> {
    Data(B),
    Trailers(HeaderMap),
}

// ===== impl Flush =====

impl<S> Flush<S>
where
    S: Body,
    S::Error: Into<Box<dyn std::error::Error>>,
{
    pub fn new(src: S, dst: SendStream<SendBuf<S::Data>>) -> Self {
        Flush {
            h2: dst,
            body: src,
            state: FlushState::Data,
        }
    }

    /// Try to flush the body.
    fn poll_complete(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), h2::Error>> {
        use self::DataOrTrailers::*;

        loop {
            match ready!(self.poll_body(cx)) {
                Some(Ok(Data(buf))) => {
                    let eos = self.body.is_end_stream();

                    self.h2.send_data(SendBuf::new(buf), eos)?;

                    if eos {
                        self.state = FlushState::Done;
                        return Ok(()).into();
                    }
                }
                Some(Ok(Trailers(trailers))) => {
                    self.h2.send_trailers(trailers)?;
                    return Ok(()).into();
                }
                Some(Err(e)) => panic!("error {:?}", e),
                None => {
                    // If this is hit, then an EOS was not reached via the other
                    // paths. So, we must send an empty data frame with EOS.
                    self.h2.send_data(SendBuf::none(), true)?;

                    return Ok(()).into();
                }
            }
        }
    }

    /// Get the next message to write, either a data frame or trailers.
    fn poll_body(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<DataOrTrailers<S::Data>, h2::Error>>> {
        loop {
            match self.state {
                FlushState::Data => {
                    // Before trying to poll the next chunk, we have to see if
                    // the h2 connection has capacity. We do this by requesting
                    // a single byte (since we don't know how big the next chunk
                    // will be.
                    self.h2.reserve_capacity(1);

                    if self.h2.capacity() == 0 {
                        // TODO: The loop should not be needed once
                        // carllerche/h2#270 is fixed.
                        loop {
                            match ready!(self.h2.poll_capacity(cx)) {
                                Some(Ok(0)) => {}
                                Some(Ok(_)) => break,
                                Some(Err(e)) => return panic!("error {:?}", e),
                                None => {
                                    debug!("connection closed early");
                                    // The error shouldn't really matter at this
                                    // point as the peer has disconnected, the
                                    // error will be discarded anyway.
                                    return Some(Err(h2::Reason::INTERNAL_ERROR.into())).into();
                                }
                            }
                        }
                    } else {
                        // If there was capacity already assigned, then the
                        // stream state wasn't polled, but we should fail out
                        // if the stream has been reset, so we poll for that.
                        match self.h2.poll_reset(cx) {
                            Poll::Ready(Ok(reason)) => {
                                debug!("stream received RST_STREAM while flushing: {:?}", reason,);
                                return Some(Err(reason.into())).into();
                            }
                            Poll::Ready(Err(e)) => return Some(Err(e)).into(),
                            Poll::Pending => {
                                // Stream hasn't been reset, so we can try
                                // to send data below. This task has been
                                // registered in case data isn't ready
                                // before we get a RST_STREAM.
                            }
                        }
                    }

                    let item = match ready!(self.body.poll_data(cx)) {
                        Some(Ok(d)) => Some(d),
                        Some(Err(err)) => {
                            let err = err.into();
                            debug!("user body error from poll_buf: {}", err);
                            let reason = crate::error::reason_from_dyn_error(&*err);
                            self.h2.send_reset(reason);
                            return Some(Err(reason.into())).into();
                        }
                        None => None,
                    };

                    if let Some(data) = item {
                        return Some(Ok(DataOrTrailers::Data(data))).into();
                    } else {
                        // Release all capacity back to the connection
                        self.h2.reserve_capacity(0);
                        self.state = FlushState::Trailers;
                    }
                }
                FlushState::Trailers => {
                    match self.h2.poll_reset(cx) {
                        Poll::Ready(Ok(reason)) => {
                            debug!(
                                "stream received RST_STREAM while flushing trailers: {:?}",
                                reason,
                            );
                            return Some(Err(reason.into())).into();
                        }
                        Poll::Ready(Err(e)) => return Some(Err(e)).into(),
                        Poll::Pending => {
                            // Stream hasn't been reset, so we can try
                            // to send data below. This task has been
                            // registered in case data isn't ready
                            // before we get a RST_STREAM.
                        }
                    }
                    let trailers = ready!(self.body.poll_trailers(cx).map_err(|err| {
                        let err = err.into();
                        debug!("user body error from poll_trailers: {}", err);
                        let reason = crate::error::reason_from_dyn_error(&*err);
                        self.h2.send_reset(reason);
                        reason
                    }))?;
                    self.state = FlushState::Done;
                    if let Some(trailers) = trailers {
                        return Some(Ok(DataOrTrailers::Trailers(trailers))).into();
                    }
                }
                FlushState::Done => return None.into(),
            }
        }
    }
}

impl<S> Future for Flush<S>
where
    S: Body + Unpin,
    S::Error: Into<Box<dyn std::error::Error>>,
{
    type Output = Result<(), ()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self)
            .poll_complete(cx)
            .map_err(|err| warn!("error flushing stream: {:?}", err))
    }
}
