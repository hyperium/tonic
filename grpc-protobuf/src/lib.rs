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

mod client;
pub use client::bidi::*;
pub use client::client_streaming::*;
pub use client::server_streaming::*;
pub use client::unary::*;
pub use client::*;

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
