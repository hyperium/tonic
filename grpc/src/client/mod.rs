/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 *
 */

use std::fmt::Display;

pub mod channel;
pub mod service;
pub(crate) mod load_balancing;
pub(crate) mod name_resolution;
pub mod service_config;

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
#[derive(Copy, Clone, PartialEq, Debug)]
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
