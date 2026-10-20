/*
 *
 * Copyright 2026 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

//! Adapts a [`LoadBalancer`] to the service shape a
//! [`ClusterChannel`](crate::client::cluster::ClusterChannel) buffers.
//!
//! The balancer serves `Error = LbError` with its own future type, while the
//! channel stack is uniformly `Error = BoxError` with a boxed future. This
//! module bridges the two.
//!
//! [`LoadBalancer`]: crate::client::loadbalance::loadbalancer::LoadBalancer

use std::task::{Context, Poll};

use tower::{BoxError, Service};

use crate::client::loadbalance::errors::LbError;
use crate::common::async_util::BoxFuture;

/// Wraps a load balancer so it can back a `ClusterChannel`.
pub(crate) struct ClusterChannelAdapter<S> {
    inner: S,
}

impl<S> ClusterChannelAdapter<S> {
    pub(crate) fn new(inner: S) -> Self {
        Self { inner }
    }
}

/// Flattens an [`LbError`] into the channel stack's `BoxError`.
///
/// Errors raised by the endpoint itself are forwarded unwrapped, so they reach
/// the retry layer exactly as the Tower balancer path delivers them — its
/// `is_retryable_connection_error` walks `Error::source`, and `LbError` does
/// not re-expose its inner error as a source. Errors the balancer raises on its
/// own behalf carry no such chain, so they convert to the status
/// [`LbError`] maps them to.
fn to_box_error(err: LbError) -> BoxError {
    match err {
        LbError::LbChannelCallError(inner) | LbError::LbChannelPollReadyError(inner) => inner,
        other => BoxError::from(tonic::Status::from(other)),
    }
}

impl<S, Req, Resp> Service<Req> for ClusterChannelAdapter<S>
where
    S: Service<Req, Response = Resp, Error = LbError>,
    S::Future: Send + 'static,
    Resp: 'static,
{
    type Response = Resp;
    type Error = BoxError;
    type Future = BoxFuture<Result<Resp, BoxError>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(to_box_error)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        let fut = self.inner.call(req);
        Box::pin(async move { fut.await.map_err(to_box_error) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::retry::is_retryable_connection_error;
    use std::io;

    #[test]
    fn endpoint_errors_keep_their_source_chain() {
        let io_err = io::Error::new(io::ErrorKind::ConnectionRefused, "refused");
        let boxed = to_box_error(LbError::LbChannelCallError(BoxError::from(io_err)));

        assert!(
            is_retryable_connection_error(boxed.as_ref()),
            "endpoint errors must stay walkable so connection retries still fire",
        );
    }

    #[test]
    fn poll_ready_errors_keep_their_source_chain() {
        let io_err = io::Error::new(io::ErrorKind::ConnectionRefused, "refused");
        let boxed = to_box_error(LbError::LbChannelPollReadyError(BoxError::from(io_err)));

        assert!(is_retryable_connection_error(boxed.as_ref()));
    }

    #[test]
    fn balancer_errors_become_statuses() {
        let boxed = to_box_error(LbError::Unavailable);
        let status = boxed
            .downcast::<tonic::Status>()
            .expect("balancer-raised errors convert to a tonic Status");
        assert_eq!(status.code(), tonic::Code::Unavailable);
    }

    #[test]
    fn stagnation_becomes_unavailable_status() {
        let boxed = to_box_error(LbError::Stagnation);
        let status = boxed.downcast::<tonic::Status>().unwrap();
        assert_eq!(status.code(), tonic::Code::Unavailable);
    }

    #[test]
    fn failed_precondition_is_preserved() {
        let boxed = to_box_error(LbError::FailedPrecondition("bug"));
        let status = boxed.downcast::<tonic::Status>().unwrap();
        assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    }
}
