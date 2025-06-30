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

use std::{any::Any, str::FromStr, sync::Arc};

use url::Url;

use crate::service::{Request, Response};

use super::ConnectivityState;

#[derive(Default)]
pub struct ChannelOptions {}

impl ChannelOptions {}

#[derive(Clone)]
pub struct Channel {
    inner: Arc<PersistentChannel>,
}

impl Channel {
    /// Constructs a new gRPC channel.  A gRPC channel is a virtual, persistent
    /// connection to a service.  Channel creation cannot fail, but if the
    /// target string is invalid, the returned channel will never connect, and
    /// will fail all RPCs.
    // TODO: should this return a Result instead?
    pub fn new(
        target: &str,
        credentials: Option<Box<dyn Any>>, // TODO: Credentials trait
        runtime: Option<Box<dyn Any>>,     // TODO: Runtime trait
        options: ChannelOptions,
    ) -> Self {
        Self {
            inner: Arc::new(PersistentChannel::new(
                target,
                credentials,
                runtime,
                options,
            )),
        }
    }

    /// Returns the current state of the channel.
    // TODO: replace with a watcher that provides state change updates?
    pub fn state(&mut self, _connect: bool) -> ConnectivityState {
        todo!()
    }

    /// Performs an RPC on the channel.  Response will contain any response
    /// messages from the server and/or errors returned by the server or
    /// generated locally.
    pub async fn call(&self, _request: Request) -> Response {
        todo!() // create the active channel if necessary and call it.
    }
}

// A PersistentChannel represents the static configuration of a channel and an
// optional Arc of an ActiveChannel.  An ActiveChannel exists whenever the
// PersistentChannel is not IDLE.  Every channel is IDLE at creation, or after
// some configurable timeout elapses without any any RPC activity.
struct PersistentChannel {
    target: Url,
    options: ChannelOptions,
    // TODO: active_channel: Mutex<Option<Arc<ActiveChannel>>>,
}

impl PersistentChannel {
    // Channels begin idle so new is a simple constructor.  Required parameters
    // are not in ChannelOptions.
    fn new(
        target: &str,
        _credentials: Option<Box<dyn Any>>,
        _runtime: Option<Box<dyn Any>>,
        options: ChannelOptions,
    ) -> Self {
        Self {
            target: Url::from_str(target).unwrap(), // TODO handle err
            options,
        }
    }
}
