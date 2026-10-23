/*
 *
 * Copyright 2025 gRPC authors.
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

use std::error::Error;

use super::{BoxError, GrpcWebService};
use tonic::body::Body;

use tower_layer::Layer;
use tower_service::Service;

/// Layer implementing the grpc-web protocol.
#[derive(Debug)]
pub struct GrpcWebLayer<ResBody = Body> {
    _markers: std::marker::PhantomData<fn() -> ResBody>,
}

impl<ResBody> Clone for GrpcWebLayer<ResBody> {
    fn clone(&self) -> Self {
        Self {
            _markers: std::marker::PhantomData,
        }
    }
}

impl<ResBody> GrpcWebLayer<ResBody> {
    /// Create a new grpc-web layer.
    pub fn new() -> Self {
        Self {
            _markers: std::marker::PhantomData,
        }
    }
}

impl<ResBody> Default for GrpcWebLayer<ResBody> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S, ResBody> Layer<S> for GrpcWebLayer<ResBody>
where
    S: Service<http::Request<Body>, Response = http::Response<ResBody>> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError> + Send,
    ResBody: http_body::Body<Data = bytes::Bytes> + Send + 'static,
    ResBody::Error: Error + Send + Sync + 'static,
{
    type Service = GrpcWebService<S, ResBody>;

    fn layer(&self, inner: S) -> Self::Service {
        GrpcWebService::new(inner)
    }
}
