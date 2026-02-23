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

pub mod channel;
pub mod service_config;
mod subchannel;
pub use channel::Channel;
pub use channel::ChannelOptions;

pub(crate) mod load_balancing;
pub(crate) mod name_resolution;
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
