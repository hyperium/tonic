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

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;

use crate::client::load_balancing::LbPolicyBuilder;

/// A registry to store and retrieve LB policies.  LB policies are indexed by
/// their names.
pub(crate) struct LbPolicyRegistry {
    m: Arc<Mutex<HashMap<String, Arc<dyn LbPolicyBuilder>>>>,
}

impl LbPolicyRegistry {
    /// Construct an empty LB policy registry.
    pub fn new() -> Self {
        Self { m: Arc::default() }
    }
    /// Add a LB policy into the registry.
    pub(crate) fn add_builder(&self, builder: impl LbPolicyBuilder + 'static) {
        self.m
            .lock()
            .unwrap()
            .insert(builder.name().to_string(), Arc::new(builder));
    }
    /// Retrieve a LB policy from the registry, or None if not found.
    pub(crate) fn get_policy(&self, name: &str) -> Option<Arc<dyn LbPolicyBuilder>> {
        self.m.lock().unwrap().get(name).cloned()
    }
}

impl Default for LbPolicyRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// The registry used if a local registry is not provided to a channel or if it
/// does not exist in the local registry.
pub(crate) static GLOBAL_LB_REGISTRY: LazyLock<LbPolicyRegistry> =
    LazyLock::new(LbPolicyRegistry::new);
