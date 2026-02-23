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
use std::error::Error;
use std::fmt::Debug;
use std::hash::Hash;
use std::sync::Arc;

use serde::Deserialize;
use serde::Serialize;
use tokio::sync::mpsc;
use tokio::sync::Notify;

use crate::client::load_balancing::ChannelController;
use crate::client::load_balancing::ForwardingSubchannel;
use crate::client::load_balancing::LbPolicy;
use crate::client::load_balancing::LbPolicyBuilder;
use crate::client::load_balancing::LbPolicyOptions;
use crate::client::load_balancing::LbState;
use crate::client::load_balancing::ParsedJsonLbConfig;
use crate::client::load_balancing::Subchannel;
use crate::client::load_balancing::SubchannelState;
use crate::client::load_balancing::WorkScheduler;
use crate::client::name_resolution::Address;
use crate::client::name_resolution::ResolverUpdate;
use crate::client::service_config::LbConfig;
use crate::service::Message;
use crate::service::Request;

#[derive(Debug)]
pub(crate) struct EmptyMessage {}
pub(crate) fn new_request() -> Request {
    Request::new(Box::pin(tokio_stream::once(
        Box::new(EmptyMessage {}) as Box<dyn Message>
    )))
}

// A test subchannel that forwards connect calls to a channel.
// This allows tests to verify when a subchannel is asked to connect.
pub(crate) struct TestSubchannel {
    address: Address,
    tx_connect: mpsc::UnboundedSender<TestEvent>,
}

impl TestSubchannel {
    pub fn new(address: Address, tx_connect: mpsc::UnboundedSender<TestEvent>) -> Self {
        Self {
            address,
            tx_connect,
        }
    }
}

impl ForwardingSubchannel for TestSubchannel {
    fn delegate(&self) -> Arc<dyn Subchannel> {
        panic!("unsupported operation on a test subchannel");
    }

    fn address(&self) -> Address {
        self.address.clone()
    }

    fn connect(&self) {
        println!("connect called for subchannel {}", self.address);
        self.tx_connect
            .send(TestEvent::Connect(self.address.clone()))
            .unwrap();
    }
}

impl Hash for TestSubchannel {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.address.hash(state);
    }
}

impl PartialEq for TestSubchannel {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}
impl Eq for TestSubchannel {}

pub(crate) enum TestEvent {
    NewSubchannel(Arc<dyn Subchannel>),
    UpdatePicker(LbState),
    RequestResolution,
    Connect(Address),
    ScheduleWork,
}

// TODO(easwars): Remove this and instead derive Debug.
impl Debug for TestEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NewSubchannel(sc) => write!(f, "NewSubchannel({})", sc.address()),
            Self::UpdatePicker(state) => write!(f, "UpdatePicker({})", state.connectivity_state),
            Self::RequestResolution => write!(f, "RequestResolution"),
            Self::Connect(addr) => write!(f, "Connect({:?})", addr.address),
            Self::ScheduleWork => write!(f, "ScheduleWork"),
        }
    }
}

/// A test channel controller that forwards calls to a channel.  This allows
/// tests to verify when a channel controller is asked to create subchannels or
/// update the picker.
pub(crate) struct TestChannelController {
    pub(crate) tx_events: mpsc::UnboundedSender<TestEvent>,
}

impl ChannelController for TestChannelController {
    fn new_subchannel(&mut self, address: &Address) -> Arc<dyn Subchannel> {
        println!("new_subchannel called for address {}", address);
        let notify = Arc::new(Notify::new());
        let subchannel: Arc<dyn Subchannel> =
            Arc::new(TestSubchannel::new(address.clone(), self.tx_events.clone()));
        self.tx_events
            .send(TestEvent::NewSubchannel(subchannel.clone()))
            .unwrap();
        subchannel
    }
    fn update_picker(&mut self, update: LbState) {
        println!("picker_update called with {}", update.connectivity_state);
        self.tx_events
            .send(TestEvent::UpdatePicker(update))
            .unwrap();
    }
    fn request_resolution(&mut self) {
        self.tx_events.send(TestEvent::RequestResolution).unwrap();
    }
}

#[derive(Debug)]
pub(crate) struct TestWorkScheduler {
    pub(crate) tx_events: mpsc::UnboundedSender<TestEvent>,
}

impl WorkScheduler for TestWorkScheduler {
    fn schedule_work(&self) {
        self.tx_events.send(TestEvent::ScheduleWork).unwrap();
    }
}

// The callback to invoke when resolver_update is invoked on the stub policy.
type ResolverUpdateFn = Arc<
    dyn Fn(
            &mut StubPolicyData,
            ResolverUpdate,
            Option<&LbConfig>,
            &mut dyn ChannelController,
        ) -> Result<(), Box<dyn Error + Send + Sync>>
        + Send
        + Sync,
>;

// The callback to invoke when subchannel_update is invoked on the stub policy.
type SubchannelUpdateFn = Arc<
    dyn Fn(&mut StubPolicyData, Arc<dyn Subchannel>, &SubchannelState, &mut dyn ChannelController)
        + Send
        + Sync,
>;

type WorkFn = Arc<dyn Fn(&mut StubPolicyData, &mut dyn ChannelController) + Send + Sync>;

/// This struct holds `LbPolicy` trait stub functions that tests are expected to
/// implement.
#[derive(Clone)]
pub(crate) struct StubPolicyFuncs {
    pub resolver_update: Option<ResolverUpdateFn>,
    pub subchannel_update: Option<SubchannelUpdateFn>,
    pub work: Option<WorkFn>,
}

impl Debug for StubPolicyFuncs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "stub funcs")
    }
}

/// Data holds test data that will be passed all to functions in PolicyFuncs
#[derive(Debug)]
pub(crate) struct StubPolicyData {
    pub lb_policy_options: LbPolicyOptions,
    pub test_data: Option<Box<dyn Any + Send + Sync>>,
}

impl StubPolicyData {
    /// Creates an instance of StubPolicyData.
    pub fn new(lb_policy_options: LbPolicyOptions) -> Self {
        Self {
            test_data: None,
            lb_policy_options,
        }
    }
}

/// The stub `LbPolicy` that calls the provided functions.
#[derive(Debug)]
pub(crate) struct StubPolicy {
    funcs: StubPolicyFuncs,
    data: StubPolicyData,
}

impl LbPolicy for StubPolicy {
    fn resolver_update(
        &mut self,
        update: ResolverUpdate,
        config: Option<&LbConfig>,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        if let Some(f) = &mut self.funcs.resolver_update {
            return f(&mut self.data, update, config, channel_controller);
        }
        Ok(())
    }

    fn subchannel_update(
        &mut self,
        subchannel: Arc<dyn Subchannel>,
        state: &SubchannelState,
        channel_controller: &mut dyn ChannelController,
    ) {
        if let Some(f) = &self.funcs.subchannel_update {
            f(&mut self.data, subchannel, state, channel_controller);
        }
    }

    fn exit_idle(&mut self, channel_controller: &mut dyn ChannelController) {
        todo!("Implement exit_idle for StubPolicy")
    }

    fn work(&mut self, channel_controller: &mut dyn ChannelController) {
        if let Some(f) = &self.funcs.work {
            f(&mut self.data, channel_controller);
        }
    }
}

/// StubPolicyBuilder builds a StubLbPolicy.
#[derive(Debug)]
pub(crate) struct StubPolicyBuilder {
    name: &'static str,
    funcs: StubPolicyFuncs,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub(super) struct MockConfig {
    shuffle_address_list: Option<bool>,
}

impl LbPolicyBuilder for StubPolicyBuilder {
    fn build(&self, options: LbPolicyOptions) -> Box<dyn LbPolicy> {
        let data = StubPolicyData::new(options);
        Box::new(StubPolicy {
            funcs: self.funcs.clone(),
            data,
        })
    }

    fn name(&self) -> &'static str {
        self.name
    }

    fn parse_config(
        &self,
        config: &ParsedJsonLbConfig,
    ) -> Result<Option<LbConfig>, Box<dyn Error + Send + Sync>> {
        let cfg: MockConfig = match config.convert_to() {
            Ok(c) => c,
            Err(e) => {
                return Err(format!("failed to parse JSON config: {}", e).into());
            }
        };
        Ok(Some(LbConfig::new(cfg)))
    }
}

pub(crate) fn reg_stub_policy(name: &'static str, funcs: StubPolicyFuncs) {
    super::GLOBAL_LB_REGISTRY.add_builder(StubPolicyBuilder { name, funcs })
}
