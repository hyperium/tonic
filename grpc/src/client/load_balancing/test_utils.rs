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

use crate::client::load_balancing::{
    ChannelController, ExternalSubchannel, ForwardingSubchannel, LbPolicy, LbPolicyBuilder,
    LbPolicyOptions, LbState, ParsedJsonLbConfig, Pick, PickResult, Picker, Subchannel,
    SubchannelState, WorkScheduler, GLOBAL_LB_REGISTRY,
};
use crate::client::name_resolution::{Address, ResolverUpdate};
use crate::client::service_config::LbConfig;
use crate::client::ConnectivityState;
use crate::service::{Message, Request, Response, Service};
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::collections::HashMap;
use std::error::Error;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::{fmt::Debug, ops::Add, sync::Arc};
use tokio::sync::mpsc::Sender;
use tokio::sync::{mpsc, Notify};
use tokio::task::AbortHandle;
use tonic::metadata::MetadataMap;

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

pub(crate) struct TestWorkScheduler {
    pub(crate) tx_events: mpsc::UnboundedSender<TestEvent>,
}

impl WorkScheduler for TestWorkScheduler {
    fn schedule_work(&self) {
        self.tx_events.send(TestEvent::ScheduleWork).unwrap();
    }
}

/// This struct holds `LbPolicy` trait stub functions that tests are expected to implement.
#[derive(Clone, Default)]
pub struct PolicyFuncs {
    pub resolver_update: Option<
        Arc<
            dyn Fn(
                    &mut Data,
                    ResolverUpdate,
                    Option<&LbConfig>,
                    &mut dyn ChannelController,
                ) -> Result<(), Box<dyn Error + Send + Sync>>
                + Send
                + Sync,
        >,
    >,
    pub subchannel_update: Option<
        Arc<
            dyn Fn(&mut Data, Arc<dyn Subchannel>, &SubchannelState, &mut dyn ChannelController)
                + Send
                + Sync,
        >,
    >,
    pub exit_idle: Option<Arc<dyn Fn(&mut Data) + Send + Sync>>,
}

/// Data holds test data that will be passed all to functions in PolicyFuncs
#[derive(Default)]
pub struct Data {
    pub test_data: Option<Box<dyn Any + Send + Sync>>,
}

/// The stub `LbPolicy` that calls the provided functions.
pub struct StubPolicy {
    funcs: PolicyFuncs,
    data: Data,
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
        if let Some(f) = &mut self.funcs.subchannel_update {
            f(&mut self.data, subchannel, state, channel_controller);
        }
    }

    fn exit_idle(&mut self, _channel_controller: &mut dyn ChannelController) {
        if let Some(f) = &mut self.funcs.exit_idle {
            f(&mut self.data);
        }
    }

    fn work(&mut self, _channel_controller: &mut dyn ChannelController) {}
}

/// This StubPolicyBuilder builds a StubLbPolicy.
pub struct StubPolicyBuilder {
    name: &'static str,
    funcs: PolicyFuncs,
}

impl StubPolicyBuilder {
    pub fn new(name: &'static str, funcs: PolicyFuncs) -> Self {
        Self { name, funcs }
    }
}

impl LbPolicyBuilder for StubPolicyBuilder {
    fn build(&self, options: LbPolicyOptions) -> Box<dyn LbPolicy> {
        let funcs = self.funcs.clone();
        let data = Data::default();
        Box::new(StubPolicy { funcs, data })
    }

    fn name(&self) -> &'static str {
        self.name
    }

    fn parse_config(
        &self,
        _config: &ParsedJsonLbConfig,
    ) -> Result<Option<LbConfig>, Box<dyn Error + Send + Sync>> {
        Ok(None)
    }
}
