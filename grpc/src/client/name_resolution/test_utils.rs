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

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::client::name_resolution::ChannelController;
use crate::client::name_resolution::ResolverUpdate;
use crate::client::name_resolution::WorkScheduler;
use crate::client::service_config::ServiceConfig;

/// A work scheduler for testing.
pub(crate) struct TestWorkScheduler {
    work_tx: mpsc::UnboundedSender<()>,
}

impl TestWorkScheduler {
    /// Creates a new `TestWorkScheduler`.
    pub(crate) fn new_pair() -> (Arc<dyn WorkScheduler>, mpsc::UnboundedReceiver<()>) {
        let (work_tx, work_rx) = mpsc::unbounded_channel();
        let sched = Self { work_tx };
        (Arc::new(sched), work_rx)
    }
}

impl WorkScheduler for TestWorkScheduler {
    fn schedule_work(&self) {
        self.work_tx.send(()).unwrap();
    }
}

/// A channel controller for testing.
pub(crate) struct TestChannelController {
    update_result: Result<(), String>,
    update_tx: mpsc::UnboundedSender<ResolverUpdate>,
}

impl TestChannelController {
    /// Creates a new `TestChannelController` that returns `Ok(())` on update.
    pub(crate) fn new_pair() -> (Self, mpsc::UnboundedReceiver<ResolverUpdate>) {
        let (update_tx, update_rx) = mpsc::unbounded_channel();
        let cc = Self {
            update_result: Ok(()),
            update_tx,
        };
        (cc, update_rx)
    }

    pub(crate) fn set_update_result(&mut self, update_result: Result<(), String>) {
        self.update_result = update_result
    }
}

impl ChannelController for TestChannelController {
    fn update(&mut self, update: ResolverUpdate) -> Result<(), String> {
        println!("Received resolver update: {:?}", &update);
        self.update_tx.send(update).unwrap();
        self.update_result.clone()
    }

    fn parse_service_config(&self, _: &str) -> Result<ServiceConfig, String> {
        Err("Unimplemented".to_string())
    }
}
