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

use tokio::sync::mpsc::UnboundedSender;

use crate::client::name_resolution::ChannelController;
use crate::client::name_resolution::ResolverUpdate;
use crate::client::name_resolution::WorkScheduler;
use crate::client::service_config::ServiceConfig;

/// A work scheduler for testing.
pub struct TestWorkScheduler {
    pub work_tx: UnboundedSender<()>,
}

impl TestWorkScheduler {
    /// Creates a new `TestWorkScheduler`.
    pub fn new(work_tx: UnboundedSender<()>) -> Self {
        Self { work_tx }
    }
}

impl WorkScheduler for TestWorkScheduler {
    fn schedule_work(&self) {
        self.work_tx.send(()).unwrap();
    }
}

/// A channel controller for testing.
pub struct TestChannelController {
    pub update_result: Result<(), String>,
    pub update_tx: UnboundedSender<ResolverUpdate>,
}

impl TestChannelController {
    /// Creates a new `TestChannelController` that returns `Ok(())` on update.
    pub fn new(update_tx: UnboundedSender<ResolverUpdate>) -> Self {
        Self {
            update_result: Ok(()),
            update_tx,
        }
    }

    /// Creates a new `TestChannelController` with a specified update result.
    pub fn new_with_result(
        update_tx: UnboundedSender<ResolverUpdate>,
        update_result: Result<(), String>,
    ) -> Self {
        Self {
            update_result,
            update_tx,
        }
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
