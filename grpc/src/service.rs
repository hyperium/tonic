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

use std::{any::Any, fmt::Debug, pin::Pin};

use bytes::{BufMut, Bytes, BytesMut};
use tokio_stream::Stream;
use tonic::{async_trait, Request as TonicRequest, Response as TonicResponse, Status};

pub type Request = TonicRequest<Pin<Box<dyn Stream<Item = Box<dyn Message>> + Send + Sync>>>;
pub type Response =
    TonicResponse<Pin<Box<dyn Stream<Item = Result<Box<dyn Message>, Status>> + Send>>>;

#[async_trait]
pub trait Service: Send + Sync {
    async fn call(
        &self,
        method: String,
        request: Request,
        response_allocator: Box<dyn MessageAllocator>,
    ) -> Response;
}

pub trait Message: Any + Send + Sync + Debug {
    /// Encodes the message into the provided buffer.
    fn encode(&self, buf: &mut BytesMut) -> Result<(), String>;
    /// Decodes the message from the provided buffer.
    fn decode(&mut self, buf: &Bytes) -> Result<(), String>;
    /// Provides a hint for the expected size of the encoded message.
    ///
    /// This method can be used by encoders to pre-allocate buffer space,
    /// potentially improving performance by reducing reallocations. It's a
    /// best-effort hint and implementations may return `None` if an
    /// accurate size cannot be easily determined without encoding.
    fn encoded_message_size_hint(&self) -> Option<usize> {
        None
    }
}

/// Allocates messages for responses on the client side and requests on the
/// server.
pub trait MessageAllocator: Send + Sync {
    fn allocate(&self) -> Box<dyn Message>;
}
