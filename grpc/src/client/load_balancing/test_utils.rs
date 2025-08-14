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
    SubchannelState, WorkScheduler,
};
use crate::client::name_resolution::Address;
use crate::client::service_config::LbConfig;
use crate::client::ConnectivityState;
use crate::service::{Message, Request, Response, Service};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::error::Error;
use std::hash::{Hash, Hasher};
use std::{fmt::Debug, ops::Add, sync::Arc};
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
    fn new(address: Address, tx_connect: mpsc::UnboundedSender<TestEvent>) -> Self {
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
            Self::Connect(addr) => write!(f, "Connect({})", addr.address.to_string()),
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

pub(crate) struct FakeChannel {
    pub tx_events: mpsc::UnboundedSender<TestEvent>,
}

impl ChannelController for FakeChannel {
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

#[derive(Clone)]
struct TestSubchannelData {
    state: Option<SubchannelState>,
}

impl TestSubchannelData {
    fn new() -> TestSubchannelData {
        TestSubchannelData { state: None }
    }
}

struct TestSubchannelList {
    subchannels: HashMap<Arc<dyn Subchannel>, TestSubchannelData>,
}

impl TestSubchannelList {
    fn new(addresses: &Vec<Address>, channel_controller: &mut dyn ChannelController) -> Self {
        let mut scl = TestSubchannelList {
            subchannels: HashMap::new(),
        };
        for address in addresses {
            let sc = channel_controller.new_subchannel(address);
            scl.subchannels.insert(sc, TestSubchannelData::new());
        }
        scl
    }

    fn subchannel_data(&self, sc: &Arc<dyn Subchannel>) -> Option<TestSubchannelData> {
        self.subchannels.get(sc).cloned()
    }

    fn contains(&self, sc: &Arc<dyn Subchannel>) -> bool {
        self.subchannels.contains_key(sc)
    }

    // Returns old state corresponding to the subchannel, if one exists.
    fn update_subchannel_data(
        &mut self,
        sc: &Arc<dyn Subchannel>,
        state: &SubchannelState,
    ) -> Option<SubchannelState> {
        let sc_data = self.subchannels.get_mut(sc).unwrap();
        let old_state = sc_data.state.clone();
        sc_data.state = Some(state.clone());
        old_state
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub(super) struct MockConfig {
    shuffle_address_list: Option<bool>,
}

/// Mock LbPolicy for testing.
pub struct MockLbPolicy {
    connectivity_state: ConnectivityState,
    name: &'static str,
    subchannel_list: Option<TestSubchannelList>,
}

impl MockLbPolicy {
    pub fn new(connectivity_state: ConnectivityState) -> Self {
        Self {
            connectivity_state,
            name: "whatever",
            subchannel_list: None,
        }
    }
}

impl LbPolicy for MockLbPolicy {
    fn resolver_update(
        &mut self,
        update: crate::client::name_resolution::ResolverUpdate,
        config: Option<&crate::client::service_config::LbConfig>,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Ok(ref endpoints) = update.endpoints {
            let addresses: Vec<_> = endpoints
                .iter()
                .flat_map(|ep| ep.addresses.clone())
                .collect();
            let scl = TestSubchannelList::new(&addresses, channel_controller);
            self.subchannel_list = Some(scl);
        }
        channel_controller.update_picker(LbState {
            connectivity_state: self.connectivity_state,
            picker: Arc::new(MockPicker { name: self.name }),
        });
        Ok(())
    }

    fn subchannel_update(
        &mut self,
        subchannel: Arc<dyn Subchannel>,
        state: &super::SubchannelState,
        channel_controller: &mut dyn ChannelController,
    ) {
        if let Some(ref mut scl) = self.subchannel_list {
            channel_controller.update_picker(LbState {
                connectivity_state: state.connectivity_state,
                picker: Arc::new(MockPicker { name: self.name }),
            });
        }
    }

    fn work(&mut self, channel_controller: &mut dyn ChannelController) {
        todo!()
    }

    fn exit_idle(&mut self, channel_controller: &mut dyn ChannelController) {
        todo!()
    }
}

/// Mock Policy Builder for testing.
pub struct MockPolicyBuilder {
    pub(super) name: &'static str,
}

impl LbPolicyBuilder for MockPolicyBuilder {
    fn build(&self, options: LbPolicyOptions) -> Box<dyn LbPolicy> {
        Box::new(MockLbPolicy {
            subchannel_list: None,
            name: self.name,
            connectivity_state: ConnectivityState::Connecting,
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

/// Mock Picker for testing purposes.
pub struct MockPicker {
    name: &'static str,
}

impl MockPicker {
    pub fn new(name: &'static str) -> Self {
        Self { name }
    }
}
impl Picker for MockPicker {
    fn pick(&self, _req: &Request) -> PickResult {
        PickResult::Pick(Pick {
            subchannel: Arc::new(TestSubchannel::new(
                Address {
                    address: self.name.to_string().into(),
                    ..Default::default()
                },
                mpsc::unbounded_channel().0,
            )),
            on_complete: None,
            metadata: MetadataMap::new(),
        })
    }
}
