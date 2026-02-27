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

use std::any::Any;
use std::sync::Arc;

/// An in-memory representation of a service config, usually provided to gRPC as
/// a JSON object.
#[derive(Debug, Default, Clone)]
pub(crate) struct ServiceConfig;

/// A convenience wrapper for an LB policy's configuration object.
#[derive(Debug, Clone)]
pub(crate) struct LbConfig {
    config: Arc<dyn Any + Send + Sync>,
}

impl LbConfig {
    /// Create a new LbConfig wrapper containing the provided config.
    pub fn new(config: impl Any + Send + Sync) -> Self {
        LbConfig {
            config: Arc::new(config),
        }
    }

    /// Convenience method to extract the LB policy's configuration object.
    pub fn convert_to<T: 'static + Send + Sync>(&self) -> Option<Arc<T>> {
        self.config.clone().downcast::<T>().ok()
    }
}
