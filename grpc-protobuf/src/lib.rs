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

//! Protobuf integration for the [`grpc`] crate.
//!
//! These types are generally created by the generated code produced by
//! [`protoc-gen-grpc-rust`](https://docs.rs/protoc-gen-grpc-rust).  See our
//! [Quick Start Guide](docs/languages/rust/quickstart/) for more information.
//!
//! # Modules
//!
//! * [`client`] - Client-side types and call builders for RPCs.
//! * [`server`] - Server-side types, handler traits, and adapters for RPCs.

use std::any::TypeId;

use bytes::Buf;
use bytes::Bytes;
use grpc::core::MessageType;
use grpc::core::RecvMessage;
use grpc::core::SendMessage;
use protobuf::AsMut;
use protobuf::AsView;
use protobuf::ClearAndParse;
use protobuf::Message;
use protobuf::MutProxied;
use protobuf::Proxied;
use protobuf::Serialize;

pub mod client;
pub mod server;
mod status;
mod trailers_conv;
pub use status::*;

/// Implements [`SendMessage`] for protobuf message views.
pub struct ProtoSendMessage<'a, V: Proxied>(V::View<'a>);

impl<'a, V: Proxied> ProtoSendMessage<'a, V> {
    pub fn from_view(provider: &'a impl AsView<Proxied = V>) -> Self {
        Self(provider.as_view())
    }
}

impl<'a, V> SendMessage for ProtoSendMessage<'a, V>
where
    V: Proxied,
    V::View<'a>: Serialize + Send + Sync,
{
    fn encode(&self) -> Result<Box<dyn Buf + Send + Sync>, String> {
        Ok(Box::new(Bytes::from(
            self.0.serialize().map_err(|e| e.to_string())?,
        )))
    }

    unsafe fn _ptr_for(&self, id: TypeId) -> Option<*const ()> {
        if id != TypeId::of::<V::View<'static>>() {
            return None;
        }
        Some(&self.0 as *const _ as *const ())
    }
}

impl<'a, V: Proxied> MessageType for ProtoSendMessage<'a, V> {
    type Target<'b> = V::View<'b>;
}

/// Implements [`RecvMessage`] for protobuf message mutable views.
pub struct ProtoRecvMessage<'a, M: MutProxied>(M::Mut<'a>);

impl<'a, M: MutProxied> ProtoRecvMessage<'a, M> {
    pub fn from_mut(provider: &'a mut impl AsMut<MutProxied = M>) -> Self {
        Self(provider.as_mut())
    }
}

impl<'a, M> RecvMessage for ProtoRecvMessage<'a, M>
where
    M: MutProxied,
    M::Mut<'a>: Send + Sync + ClearAndParse,
{
    fn decode(&mut self, buf: &mut dyn Buf) -> Result<(), String> {
        let len = buf.remaining();

        if buf.chunk().len() == len {
            self.0
                .clear_and_parse(buf.chunk())
                .map_err(|e| e.to_string())?;
        } else {
            let mut temp_vec = vec![0u8; len];
            buf.copy_to_slice(&mut temp_vec);
            self.0
                .clear_and_parse(&temp_vec)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    unsafe fn _ptr_for(&mut self, id: TypeId) -> Option<*mut ()> {
        if id != TypeId::of::<M::Mut<'static>>() {
            return None;
        }
        Some(&mut self.0 as *mut _ as *mut ())
    }
}

impl<'a, M: Message> MessageType for ProtoRecvMessage<'a, M> {
    type Target<'b> = M::Mut<'b>;
}

mod private {
    pub struct Internal;
}

/// A helper trait to enforce and explicitly bound a [`Future`] as [`Send`].
///
/// This trait provides a mechanism to work around specific Rust compiler
/// limitations and bugs where the compiler's borrow checker or drop analysis
/// conservatively concludes that an `async` block is `!Send` (not safe to send
/// across threads),
/// even when it logically should be.
///
/// # Problem Context
///
/// As detailed in issues [#64552], [#102211], and [#96865], there are scenarios
/// where:
/// * An `async` function captures a reference to a type that is `!Sync`.
/// * A variable is dropped before an `.await` point, but the compiler's liveness
///   analysis incorrectly believes it is held across the await.
/// * Complex control flow confuses the auto-trait deduction for `Send`.
///
/// These scenarios often result in obscure error messages when trying to spawn
/// the future on an executor (like `tokio::spawn`), claiming the future is not
/// `Send`.
///
/// # The Solution
///
/// The `make_send()` method acts as an identity function (a no-op at runtime) but
/// performs two critical compile-time tasks:
///
/// 1.  **Explicit Assertion:** It requires `Self` to implement `Send` at the
///     call site. This moves the error message from the deep internals of an
///     executor's spawn function to the specific line where the future is created,
///     making debugging significantly easier.
/// 2.  **Type Erasure / Coercion:** By returning `impl Future<...> + Send`, it
///     creates an opaque type boundary. This can sometimes help the compiler's
///     trait solver "lock in" the `Send` guarantee and disregard phantom lifetime
///     issues that might otherwise propagate.
///
/// [#64552]: https://github.com/rust-lang/rust/issues/64552
/// [#102211]: https://github.com/rust-lang/rust/issues/102211
/// [#96865]: https://github.com/rust-lang/rust/issues/96865
/// [`Future`]: core::future::Future
/// [`Send`]: core::marker::Send
// TODO: delete this type once MSRV is v1.92.
trait SendFuture: Future {
    /// Consumes the future and returns it as an opaque type that is guaranteed
    /// to be [`Send`].
    ///
    /// This is a zero-cost abstraction (it simply returns `self`) used primarily
    /// to help the compiler resolve auto-traits or to produce better error
    /// diagnostics.
    fn make_send(self) -> impl Future<Output = Self::Output> + Send
    where
        Self: Sized + Send,
    {
        self
    }
}

impl<T: Future> SendFuture for T {}
