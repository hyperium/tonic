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

use std::any::type_name;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;

use crate::client::load_balancing::ChannelController;
use crate::client::load_balancing::DynLbConfig;
use crate::client::load_balancing::DynLbPolicy;
use crate::client::load_balancing::DynLbPolicyBuilder;
use crate::client::load_balancing::LbPolicy;
use crate::client::load_balancing::LbPolicyBuilder;
use crate::client::load_balancing::LbPolicyOptions;
use crate::client::load_balancing::ParsedJsonLbConfig;
use crate::client::load_balancing::subchannel::Subchannel;
use crate::client::load_balancing::subchannel::SubchannelState;
use crate::client::name_resolution::ResolverUpdate;

/// A registry to store and retrieve LB policies.  LB policies are indexed by
/// their names.
pub(crate) struct LbPolicyRegistry {
    m: Arc<Mutex<HashMap<String, Arc<DynLbPolicyBuilder>>>>,
}

impl LbPolicyRegistry {
    /// Constructs an empty LB policy registry.
    pub fn new() -> Self {
        Self { m: Arc::default() }
    }

    /// Adds a LB policy into the registry.
    pub(crate) fn add_builder<B: LbPolicyBuilder>(&self, builder: B) {
        self.m
            .lock()
            .unwrap()
            .insert(builder.name().to_string(), DynAdapter::new_arc(builder));
    }

    /// Adds a dynamic LB policy into the registry.
    pub(crate) fn add_dyn_builder(&self, builder: Arc<DynLbPolicyBuilder>) {
        self.m
            .lock()
            .unwrap()
            .insert(builder.name().to_string(), builder);
    }

    /// Retrieves a LB policy from the registry, or None if not found.
    pub(crate) fn get_policy(&self, name: &str) -> Option<Arc<DynLbPolicyBuilder>> {
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

/// Implements DynLbPolicy and DynLbPolicyBuilder around the enclosed LbPolicy
/// or LbPolicyBuilder, respectively.
#[derive(Debug)]
struct DynAdapter<T>(T);

impl<T: LbPolicyBuilder> LbPolicyBuilder for DynAdapter<T> {
    type LbPolicy = Box<DynLbPolicy>;

    fn build(&self, options: LbPolicyOptions) -> Self::LbPolicy {
        Box::new(DynAdapter(self.0.build(options)))
    }

    fn name(&self) -> &'static str {
        self.0.name()
    }

    fn parse_config(&self, config: &ParsedJsonLbConfig) -> Result<Option<DynLbConfig>, String> {
        // Call the real parse config and then wrap its result in a DynLbConfig if it is Ok(Some)
        let cfg = self.0.parse_config(config)?;
        Ok(cfg.map(|c| Arc::new(c) as DynLbConfig))
    }
}

impl<T: LbPolicy> LbPolicy for DynAdapter<T> {
    type LbConfig = DynLbConfig;

    fn resolver_update(
        &mut self,
        update: ResolverUpdate,
        config: Option<&DynLbConfig>,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), String> {
        let config = config.map(|c| {
            c.downcast_ref::<T::LbConfig>().unwrap_or_else(|| {
                panic!("LB config type should be {}", type_name::<T::LbConfig>())
            })
        });
        self.0.resolver_update(update, config, channel_controller)
    }

    fn subchannel_update(
        &mut self,
        subchannel: Arc<dyn Subchannel>,
        state: &SubchannelState,
        channel_controller: &mut dyn ChannelController,
    ) {
        self.0
            .subchannel_update(subchannel, state, channel_controller);
    }

    fn work(&mut self, channel_controller: &mut dyn ChannelController) {
        self.0.work(channel_controller);
    }

    fn exit_idle(&mut self, channel_controller: &mut dyn ChannelController) {
        self.0.exit_idle(channel_controller);
    }
}

impl<T> DynAdapter<T> {
    fn new_arc(policy: T) -> Arc<Self> {
        Arc::new(DynAdapter(policy))
    }
}
