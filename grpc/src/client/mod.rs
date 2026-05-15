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

//! Client-side gRPC implementation and utilities.
//!
//! This module provides the core types and traits for building gRPC clients.
//! While most applications will use generated code (e.g. using
//! `protoc-gen-rust-grpc`) to interact with gRPC services, this module provides
//! the underlying primitives.
//!
//! # Key Concepts
//!
//! - **[`Channel`]:** The main entry point for client-side gRPC operations. It
//!   manages connections to servers and load balancing between them.
//! - **[`Invoke`]:** A trait for executing RPCs. Both [`Channel`] and
//!   references to it implement this trait.
//! - **[`SendStream`] / [`RecvStream`]:** Represent the sending and receiving
//!   sides of an RPC call, returned by [`Invoke::invoke`].

use std::fmt::Display;
use std::time::Instant;

use tonic::async_trait;

use crate::core::RecvMessage;
use crate::core::RequestHeaders;
use crate::core::ResponseHeaders;
use crate::core::SendMessage;
use crate::core::Trailers;

mod channel;
pub mod interceptor;
pub mod metadata_utils;
pub(crate) mod service_config;
pub mod stream_util;

pub use channel::Channel;
pub use channel::ChannelOptions;

pub(crate) mod load_balancing;
pub(crate) mod name_resolution;
mod subchannel;
pub(crate) mod transport;

#[cfg(test)]
mod test_util;

/// A representation of the current state of a gRPC channel, also used for the
/// state of subchannels (individual connections within the channel).
///
/// A gRPC channel begins in the `Idle` state.  When an RPC is attempted or when
/// [`Channel::get_state`] is called with `connect=true`, the channel will
/// transition to `Connecting`.  If connections to a backend service are
/// available, the state becomes `Ready`.  Otherwise, if RPCs would fail due to
/// a lack of connections, the state becomes `TransientFailure`.
///
/// Channels may re-enter the Idle state if they are unused for longer than
/// their configured idleness timeout.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum ConnectivityState {
    #[default]
    /// Represents a channel that is not currently connected to any backends.
    Idle,
    /// Represents a channel that is actively connecting to one or more backends
    /// and has no connected backends.
    Connecting,
    /// Represents a channel that is connected to at least one backend.
    Ready,
    /// Represents a channel that failed to connect to its backends.
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

/// Settings to configure RPCs sent using the [`Invoke`] trait.
///
/// Most applications will not need this type, and will set options via the
/// generated (e.g. protobuf) APIs instead.
#[derive(Default, Clone)]
#[non_exhaustive]
pub struct CallOptions {
    /// The deadline for the call.  If unset, the call may run indefinitely.
    deadline: Option<Instant>,
}

impl CallOptions {
    /// Constructs a new [`CallOptions`] with the default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies a deadline to the call; when the deadline is reached, the call
    /// is cancelled.
    pub fn set_deadline(&mut self, deadline: Instant) {
        self.deadline = Some(deadline);
    }

    /// Reads the deadline currently set in the [`CallOptions`].
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }
}

/// A trait which may be implemented by types to perform RPCs (Remote Procedure
/// Calls, often shortened to "call").
///
/// Most applications will not use this type directly, and will instead use the
/// generated APIs (e.g.  protobuf) to perform RPCs instead.
#[trait_variant::make(Send)]
pub trait Invoke: Sync {
    /// The sending stream returned by `invoke`.
    type SendStream: SendStream + 'static;
    /// The receiving stream returned by `invoke`.
    type RecvStream: RecvStream + 'static;

    /// Starts an RPC, returning the send and receive streams to interact with
    /// it.
    ///
    /// Note that invoke is asynchronous, and may block as needed if the channel
    /// is still connecting or if the connection the RPC is routed to has
    /// reached its maximum stream limit.
    async fn invoke(
        &self,
        headers: RequestHeaders,
        options: CallOptions,
    ) -> (Self::SendStream, Self::RecvStream);
}

/// A dyn-compatible version of [`Invoke`].
#[async_trait]
pub trait DynInvoke: Send + Sync {
    /// Starts an RPC, returning the send and receive streams to interact with
    /// it.
    ///
    /// Note that dyn_invoke is asynchronous, and may block as needed if the
    /// channel is still connecting or if the connection the RPC is routed to
    /// has reached its maximum stream limit.
    async fn dyn_invoke(
        &self,
        headers: RequestHeaders,
        options: CallOptions,
    ) -> (Box<dyn DynSendStream>, Box<dyn DynRecvStream>);
}

#[async_trait]
impl<T: Invoke> DynInvoke for T {
    async fn dyn_invoke(
        &self,
        headers: RequestHeaders,
        options: CallOptions,
    ) -> (Box<dyn DynSendStream>, Box<dyn DynRecvStream>) {
        let (tx, rx) = self.invoke(headers, options).await;
        (Box::new(tx), Box::new(rx))
    }
}

/// Like [`Invoke`], but not reusable.  Blanket implemented on references to
/// [`Invoke`]s.
#[trait_variant::make(Send)]
pub trait InvokeOnce: Sync {
    /// The sending stream returned by `invoke_once`.
    type SendStream: SendStream + 'static;
    /// The receiving stream returned by `invoke_once`.
    type RecvStream: RecvStream + 'static;

    /// Starts an RPC, returning the send and receive streams to interact with
    /// it.
    ///
    /// Note that invoke_once is asynchronous, and may block as needed if the
    /// channel is still connecting or if the connection the RPC is routed to
    /// has reached its maximum stream limit.
    async fn invoke_once(
        self,
        headers: RequestHeaders,
        options: CallOptions,
    ) -> (Self::SendStream, Self::RecvStream);
}

impl<T: Invoke> InvokeOnce for &T {
    type SendStream = T::SendStream;
    type RecvStream = T::RecvStream;

    async fn invoke_once(
        self,
        headers: RequestHeaders,
        options: CallOptions,
    ) -> (Self::SendStream, Self::RecvStream) {
        self.invoke(headers, options).await
    }
}

/// Represents the sending side of a client stream.  When a `SendStream` is
/// dropped, the send side of the stream is closed.  Clients may continue to
/// read from the [`RecvStream`] in this case.
///
/// Most applications will not need this type directly, and will use the
/// generated APIs (e.g.  protobuf) to perform RPCs instead.
#[trait_variant::make(Send)]
pub trait SendStream {
    /// Sends `msg` on the stream.  If `Err(())` is returned, the message could
    /// not be delivered because the stream was closed.  Future calls to
    /// SendStream will do nothing.
    ///
    /// # Cancel safety
    ///
    /// This method is not intended to be cancellation safe.  If the returned
    /// future is not polled to completion, the behavior of any subsequent calls
    /// to the SendStream are undefined and data may be lost.
    async fn send(&mut self, msg: &dyn SendMessage, options: SendOptions) -> Result<(), ()>;
}

/// A dyn-compatible version of [`SendStream`].
#[async_trait]
pub trait DynSendStream: Send {
    /// A dynamic version of [`SendStream::send`].
    async fn dyn_send(&mut self, msg: &dyn SendMessage, options: SendOptions) -> Result<(), ()>;
}

#[async_trait]
impl<T: SendStream> DynSendStream for T {
    async fn dyn_send(&mut self, msg: &dyn SendMessage, options: SendOptions) -> Result<(), ()> {
        self.send(msg, options).await
    }
}

impl<'a> SendStream for Box<dyn DynSendStream + 'a> {
    async fn send(&mut self, msg: &dyn SendMessage, options: SendOptions) -> Result<(), ()> {
        (**self).dyn_send(msg, options).await
    }
}

/// Settings for sending request messages on a client [`SendStream`].
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

impl SendOptions {
    /// Constructs a new [`SendOptions`] with the default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the "final message" flag in the options.  If true, this indicates
    /// that the message accompanying these options is the final request message
    /// on the stream, which closes the send stream and prevents sending
    /// subsequent messages.  Messages may still be received on the
    /// [`RecvStream`].
    pub fn with_final_msg(mut self, final_msg: bool) -> Self {
        self.final_msg = final_msg;
        self
    }

    /// Sets the "disable compression" flag in the options.  If compression is
    /// not already enabled on the stream, this has no effect.
    pub fn with_disable_compression(mut self, disable_compression: bool) -> Self {
        self.disable_compression = disable_compression;
        self
    }
}

/// An item in a response stream from the client's view.
///
/// A response stream must always contain items exactly as follows:
///
/// [Headers *Message] Trailers *StreamClosed
///
/// That is: optionally, a Headers value and any number of Message values
/// (including zero), followed by a required Trailers value.  A response stream
/// should not be used after Trailers, but reads should return StreamClosed if
/// it is.
#[derive(Debug, Clone)]
pub enum ResponseStreamItem {
    /// Indicates the headers for the stream.
    Headers(ResponseHeaders),
    /// Indicates a message on the stream.
    Message,
    /// Indicates trailers were received on the stream and includes the trailers.
    Trailers(Trailers),
    /// Indicates the response stream was closed.  Trailers must have been
    /// provided before this value may be used.
    StreamClosed,
}

/// Represents the receiving side of a client stream.  When a `RecvStream` is
/// dropped, the associated call is cancelled if the server has not already
/// terminated the stream.
///
/// Most applications will not need this type directly, and will use the
/// generated APIs (e.g.  protobuf) to perform RPCs instead.
#[trait_variant::make(Send)]
pub trait RecvStream {
    /// Returns the next item on the stream.  If that item represents a message,
    /// `msg` has been updated directly to contain the received message.
    ///
    /// # Cancel safety
    ///
    /// This method is not intended to be cancellation safe.  If the returned
    /// future is not polled to completion, the behavior of any subsequent calls
    /// to the RecvStream are undefined and data may be lost.
    async fn recv(&mut self, msg: &mut dyn RecvMessage) -> ResponseStreamItem;
}

/// A dyn-compatible version of [`RecvStream`].
#[async_trait]
pub trait DynRecvStream: Send {
    /// A dynamic version of [`RecvStream::recv`].
    async fn dyn_recv(&mut self, msg: &mut dyn RecvMessage) -> ResponseStreamItem;
}

#[async_trait]
impl<T: RecvStream> DynRecvStream for T {
    async fn dyn_recv(&mut self, msg: &mut dyn RecvMessage) -> ResponseStreamItem {
        self.recv(msg).await
    }
}

impl<'a> RecvStream for Box<dyn DynRecvStream + 'a> {
    async fn recv(&mut self, msg: &mut dyn RecvMessage) -> ResponseStreamItem {
        (**self).dyn_recv(msg).await
    }
}
