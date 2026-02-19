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

use std::fmt::Display;
use std::time::Instant;

use crate::core::ClientResponseStreamItem;
use crate::core::RecvMessage;
use crate::core::RequestHeaders;
use crate::core::SendMessage;

pub mod channel;
pub mod interceptor;
pub mod service_config;
pub mod stream_util;

pub use channel::Channel;
pub use channel::ChannelOptions;

pub(crate) mod load_balancing;
pub(crate) mod name_resolution;
mod subchannel;
pub(crate) mod transport;

/// A representation of the current state of a gRPC channel, also used for the
/// state of subchannels (individual connections within the channel).
///
/// A gRPC channel begins in the Idle state.  When an RPC is attempted, the
/// channel will automatically transition to Connecting.  If connections to a
/// backend service are available, the state becomes Ready.  Otherwise, if RPCs
/// would fail due to a lack of connections, the state becomes TransientFailure
/// and continues to attempt to reconnect.
///
/// Channels may re-enter the Idle state if they are unused for longer than
/// their configured idleness timeout.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ConnectivityState {
    Idle,
    Connecting,
    Ready,
    TransientFailure,
}

impl Display for ConnectivityState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectivityState::Idle => write!(f, "Idle"),
            ConnectivityState::Connecting => write!(f, "Connecting"),
            ConnectivityState::Ready => write!(f, "Ready"),
            ConnectivityState::TransientFailure => write!(f, "TransientFailure"),
        }
    }
}

/// Contains settings to configure an RPC.
///
/// Most applications will not need this type, and will set options via the
/// generated (e.g. protobuf) APIs instead.
#[derive(Default, Clone)]
#[non_exhaustive]
pub struct CallOptions {
    /// The deadline for the call.  If unset, the call may run indefinitely.
    deadline: Option<Instant>,
}

/// A trait which may be implemented by types to perform RPCs (Remote Procedure
/// Calls, often shortened to "call").
///
/// Most applications will not use this type directly, and will instead use the
/// generated APIs (e.g.  protobuf) to perform RPCs instead.
pub trait Invoke: Send + Sync {
    type SendStream: SendStream + 'static;
    type RecvStream: RecvStream + 'static;

    /// Starts an RPC, returning the send and receive streams to interact with
    /// it.
    ///
    /// Note that invoke is synchronous, which implies no pushback may be
    /// enforced via execution flow.  If a call cannot be started or queued
    /// locally, the returned SendStream and RecvStream may represent a
    /// locally-erroring stream immediately instead.  However, SendStream and
    /// RecvStream are asynchronous, and may block their first operations until
    /// quota is available, a connection is ready, etc.
    fn invoke(
        &self,
        headers: RequestHeaders,
        options: CallOptions,
    ) -> (Self::SendStream, Self::RecvStream);
}

// Like `Invoke`, but not reusable.  It is blanket implemented on references to
// `Invoke`s.
pub trait InvokeOnce: Send + Sync {
    type SendStream: SendStream + 'static;
    type RecvStream: RecvStream + 'static;

    fn invoke_once(
        self,
        headers: RequestHeaders,
        options: CallOptions,
    ) -> (Self::SendStream, Self::RecvStream);
}

impl<T: Invoke> InvokeOnce for &T {
    type SendStream = T::SendStream;
    type RecvStream = T::RecvStream;

    fn invoke_once(
        self,
        headers: RequestHeaders,
        options: CallOptions,
    ) -> (Self::SendStream, Self::RecvStream) {
        self.invoke(headers, options)
    }
}

/// Represents the sending side of a client stream.  When a `SendStream` is
/// dropped, the send side of the stream is closed.  Clients may continue to
/// read from the RecvStream
///
/// Most applications will not need this type directly, and will use the
/// generated APIs (e.g.  protobuf) to perform RPCs instead.
#[trait_variant::make(Send)]
pub trait SendStream: Send {
    /// Sends T on the stream.  If Err(()) is returned, the message could not be
    /// delivered because the stream was closed.  Future calls to SendStream
    /// will do nothing.
    ///
    /// # Cancel safety
    ///
    /// This method is not intended to be cancellation safe.  If the returned
    /// future is not polled to completion, the behavior of any subsequent calls
    /// to the SendStream are undefined and data may be lost.
    async fn send(&mut self, msg: &dyn SendMessage, options: SendOptions) -> Result<(), ()>;
}

/// Contains settings to configure a send operation on a SendStream.
///
/// Most applications will not need this type directly, and will use the
/// generated (e.g.  protobuf) APIs to configure RPCs instead.
#[derive(Default, Clone)]
#[non_exhaustive]
pub struct SendOptions {
    /// Closes the stream immediately after sending this message.
    pub final_msg: bool,
    /// If set, compression will be disabled for this message.
    pub disable_compression: bool,
}

/// Represents the receiving side of a client stream.  When a `RecvStream` is
/// dropped, the associated call is cancelled if the server has not already
/// terminated the stream.
///
/// Most applications will not need this type directly, and will use the
/// generated APIs (e.g.  protobuf) to perform RPCs instead.
#[trait_variant::make(Send)]
pub trait RecvStream: Send {
    /// Returns the next item on the stream.  If that item represents a message,
    /// `msg` has been updated directly to contain the received message.
    ///
    /// # Cancel safety
    ///
    /// This method is not intended to be cancellation safe.  If the returned
    /// future is not polled to completion, the behavior of any subsequent calls
    /// to the RecvStream are undefined and data may be lost.
    async fn next(&mut self, msg: &mut dyn RecvMessage) -> ClientResponseStreamItem;
}
