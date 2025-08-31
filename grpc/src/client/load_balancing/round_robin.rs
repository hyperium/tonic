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

use crate::client::load_balancing::child_manager::{
    self, ChildManager, ChildUpdate, ResolverUpdateSharder,
};
use crate::client::load_balancing::pick_first::{self};
use crate::client::load_balancing::utils::EndpointSharder;
use crate::client::load_balancing::{
    ChannelController, ExternalSubchannel, Failing, LbConfig, LbPolicy, LbPolicyBuilder,
    LbPolicyOptions, LbState, ParsedJsonLbConfig, Pick, PickResult, Picker, QueuingPicker,
    Subchannel, SubchannelState, WorkScheduler, GLOBAL_LB_REGISTRY,
};
use crate::client::name_resolution::{Address, Endpoint, ResolverUpdate};
use crate::client::transport::{Transport, GLOBAL_TRANSPORT_REGISTRY};
use crate::client::ConnectivityState;
use crate::rt::{default_runtime, Runtime};
use crate::service::{Message, Request, Response, Service};
use core::panic;
use rand::{self, rngs::StdRng, seq::SliceRandom, Rng, RngCore, SeedableRng};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::hash::Hash;
use std::mem;
use std::ops::Add;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once};
use tokio::sync::{mpsc, Notify};
use tonic::{async_trait, metadata::MetadataMap};

pub static POLICY_NAME: &str = "round_robin";
static START: Once = Once::new();

struct RoundRobinBuilder {}

impl LbPolicyBuilder for RoundRobinBuilder {
    fn build(&self, options: LbPolicyOptions) -> Box<dyn LbPolicy> {
        let resolver_update_sharder = EndpointSharder::new(
            GLOBAL_LB_REGISTRY
                .get_policy(pick_first::POLICY_NAME)
                .unwrap(),
        );
        let child_manager = Box::new(ChildManager::new(
            Box::new(resolver_update_sharder),
            options.runtime,
        ));
        Box::new(RoundRobinPolicy::new(child_manager, options.work_scheduler))
    }

    fn name(&self) -> &'static str {
        POLICY_NAME
    }
}

struct RoundRobinPolicy {
    child_manager: Box<ChildManager<Endpoint>>,
    work_scheduler: Arc<dyn WorkScheduler>,
    addresses_available: bool, // Whether addresses were available from the previous resolver updates.
}

impl RoundRobinPolicy {
    fn new(
        child_manager: Box<ChildManager<Endpoint>>,
        work_scheduler: Arc<dyn WorkScheduler>,
    ) -> Self {
        Self {
            child_manager,
            work_scheduler,
            addresses_available: false,
        }
    }

    // Sends aggregate picker based on states of children.
    //
    // If the aggregate state is Idle or Connecting, round robin across connecting children's pickers.
    // If the aggregate state is Ready, send the pickers of all Ready children.
    // If the aggregate state is Transient Failure, send a Transient Failure
    // picker.
    fn send_picker_with_children(
        &mut self,
        channel_controller: &mut dyn ChannelController,
        aggregate_state: ConnectivityState,
    ) {
        let mut pickers = Vec::new();
        for (idenifier, state) in self.child_manager.child_states() {
            if state.connectivity_state == aggregate_state {
                pickers.push(state.picker.clone());
            }
        }
        let picker_update = LbState {
            connectivity_state: aggregate_state,
            picker: Arc::new(RoundRobinPicker::new(pickers)),
        };
        channel_controller.update_picker(picker_update);
    }

    // Sends aggregate picker based on states of children.
    //
    // If the aggregate state is Idle or Connecting, send pickers
    // of CONNECTING children.
    // If the aggregate state is Ready, send the pickers of all Ready children.
    // If the aggregate state is Transient Failure, send pickers of TRANSIENT
    // FAILURE children.
    fn send_aggregate_picker(&mut self, channel_controller: &mut dyn ChannelController) {
        let state = self.child_manager.aggregate_states();
        println!("state is {}", state);
        match state {
            ConnectivityState::Idle | ConnectivityState::Connecting => {
                self.send_picker_with_children(channel_controller, ConnectivityState::Connecting);
            }
            ConnectivityState::Ready => {
                self.send_picker_with_children(channel_controller, ConnectivityState::Ready);
            }
            ConnectivityState::TransientFailure => {
                self.send_picker_with_children(
                    channel_controller,
                    ConnectivityState::TransientFailure,
                );
            }
        }
    }

    fn move_children_from_idle(&mut self, channel_controller: &mut dyn ChannelController) {
        let mut should_exit_idle = false;
        for (_id, state) in self.child_manager.child_states() {
            if state.connectivity_state == ConnectivityState::Idle {
                should_exit_idle = true;
            }
        }
        if should_exit_idle {
            self.child_manager.exit_idle(channel_controller);
        }
    }

    fn move_to_transient_failure(
        &mut self,
        channel_controller: &mut dyn ChannelController,
        error: String,
    ) {
        channel_controller.update_picker(LbState {
            connectivity_state: ConnectivityState::TransientFailure,
            picker: Arc::new(Failing { error }),
        });
        channel_controller.request_resolution();
    }
}

impl LbPolicy for RoundRobinPolicy {
    fn resolver_update(
        &mut self,
        update: ResolverUpdate,
        config: Option<&LbConfig>,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        match update.clone().endpoints {
            Ok(endpoints) => {
                let addresses_available = endpoints.iter().any(|ep| !ep.addresses.is_empty());
                if !addresses_available {
                    self.child_manager.forward_update_to_children(
                        channel_controller,
                        update,
                        config,
                    );
                    self.move_to_transient_failure(
                        channel_controller,
                        "received empty address list from the name resolver".into(),
                    );
                    return Err("received empty address list from the name resolver".into());
                }

                println!("resolver update");
                let result = self
                    .child_manager
                    .resolver_update(update, config, channel_controller);
                self.move_children_from_idle(channel_controller);
                if self.child_manager.has_updated() {
                    self.send_aggregate_picker(channel_controller);
                }
            }
            Err(resolver_error) => {
                println!("resolver error");
                if self.child_manager.child_states().next().is_none() {
                    self.move_to_transient_failure(channel_controller, resolver_error.clone());
                    return Err(resolver_error.into());
                } else {
                    self.child_manager.forward_update_to_children(
                        channel_controller,
                        update,
                        config,
                    );
                    self.move_children_from_idle(channel_controller);
                    if self.child_manager.has_updated() {
                        self.send_aggregate_picker(channel_controller);
                    }
                }
            }
        }
        Ok(())
    }

    fn subchannel_update(
        &mut self,
        subchannel: Arc<dyn Subchannel>,
        state: &SubchannelState,
        channel_controller: &mut dyn ChannelController,
    ) {
        self.child_manager
            .subchannel_update(subchannel, state, channel_controller);
        self.move_children_from_idle(channel_controller);
        if self.child_manager.has_updated() {
            self.send_aggregate_picker(channel_controller);
        }
    }

    fn work(&mut self, channel_controller: &mut dyn ChannelController) {
        self.child_manager.work(channel_controller);
        self.move_children_from_idle(channel_controller);
        if self.child_manager.has_updated() {
            self.send_aggregate_picker(channel_controller);
        }
    }

    fn exit_idle(&mut self, channel_controller: &mut dyn ChannelController) {
        self.child_manager.exit_idle(channel_controller);
        self.move_children_from_idle(channel_controller);
        if self.child_manager.has_updated() {
            self.send_aggregate_picker(channel_controller);
        }
    }
}

/// Register round robin as a LbPolicy.
pub fn reg() {
    START.call_once(|| {
        GLOBAL_LB_REGISTRY.add_builder(RoundRobinBuilder {});
    });
}

struct RoundRobinPicker {
    pickers: Vec<Arc<dyn Picker>>,
    next: AtomicUsize,
}

impl RoundRobinPicker {
    fn new(pickers: Vec<Arc<dyn Picker>>) -> Self {
        let random_index: usize = rand::random_range(..pickers.len());
        Self {
            pickers,
            next: AtomicUsize::new(random_index),
        }
    }
}

impl Picker for RoundRobinPicker {
    fn pick(&self, request: &Request) -> PickResult {
        let len = self.pickers.len();
        let idx = self.next.fetch_add(1, Ordering::Relaxed) % len;
        self.pickers[idx].pick(request)
    }
}

#[cfg(test)]
mod test {
    use crate::client::load_balancing::child_manager::{
        ChildManager, ChildUpdate, ResolverUpdateSharder,
    };
    use crate::client::load_balancing::round_robin::{self, RoundRobinPolicy};
    use crate::client::load_balancing::test_utils::{
        self, StubPolicy, StubPolicyData, StubPolicyFuncs, TestChannelController, TestEvent,
        TestSubchannel, TestWorkScheduler,
    };
    use crate::client::load_balancing::utils::EndpointSharder;
    use crate::client::load_balancing::{
        ChannelController, Failing, LbPolicy, LbPolicyBuilder, LbPolicyOptions, LbState,
        ParsedJsonLbConfig, Pick, PickResult, Picker, QueuingPicker, Subchannel, SubchannelState,
        GLOBAL_LB_REGISTRY,
    };
    use crate::client::name_resolution::{Address, Endpoint, ResolverUpdate};
    use crate::client::service_config::{LbConfig, ServiceConfig};
    use crate::client::ConnectivityState;
    use crate::rt::{default_runtime, Runtime};
    use crate::service::Request;
    use serde::{Deserialize, Serialize};
    use std::collections::{HashMap, HashSet};
    use std::error::Error;
    use std::panic;
    use std::sync::Arc;
    use std::sync::Mutex;
    use tokio::sync::mpsc;
    use tonic::metadata::MetadataMap;

    // Sets up the test environment.
    //
    // Performs the following:
    // 1. Creates a work scheduler.
    // 2. Creates a fake channel that acts as a channel controller.
    // 3. Creates an StubPolicyBuilder with StubFuncs and the name of the test
    //    passed in.
    // 4. Creates an EndpointSharder with StubPolicyBuilder passed in as the
    //    child policy.
    // 5. Creates a ChildManager with the EndpointSharder.
    // 6. Create a Round Robin policy with the ChildManager passed in.
    //
    // Returns the following:
    // 1. A receiver for events initiated by the LB policy (like creating a new
    //    subchannel, sending a new picker etc).
    // 2. The Round Robin to send resolver and subchannel updates from the test.
    // 3. The controller to pass to the LB policy as part of the updates.
    fn setup(
        funcs: StubPolicyFuncs,
        test_name: &'static str,
    ) -> (
        mpsc::UnboundedReceiver<TestEvent>,
        Box<dyn LbPolicy>,
        Box<dyn ChannelController>,
    ) {
        round_robin::reg();
        test_utils::reg_stub_policy(test_name, funcs);
        let resolver_update_sharder =
            EndpointSharder::new(GLOBAL_LB_REGISTRY.get_policy(test_name).unwrap());

        let child_manager = Box::new(ChildManager::new(
            Box::new(resolver_update_sharder),
            default_runtime(),
        ));
        let (tx_events, rx_events) = mpsc::unbounded_channel();
        let work_scheduler = Arc::new(TestWorkScheduler {
            tx_events: tx_events.clone(),
        });
        let tcc = Box::new(TestChannelController { tx_events });

        let lb_policy = Box::new(RoundRobinPolicy::new(child_manager, work_scheduler));
        (rx_events, lb_policy, tcc)
    }

    struct TestSubchannelList {
        subchannels: Vec<Arc<dyn Subchannel>>,
    }

    impl TestSubchannelList {
        fn new(addresses: &Vec<Address>, channel_controller: &mut dyn ChannelController) -> Self {
            let mut scl = TestSubchannelList {
                subchannels: Vec::new(),
            };
            for address in addresses {
                let sc = channel_controller.new_subchannel(address);
                scl.subchannels.push(sc.clone());
            }
            scl
        }

        fn contains(&self, sc: &Arc<dyn Subchannel>) -> bool {
            self.subchannels.contains(sc)
        }
    }

    fn create_n_endpoints_with_k_addresses(n: usize, k: usize) -> Vec<Endpoint> {
        let mut endpoints = Vec::with_capacity(n);
        for i in 0..n {
            let mut addresses: Vec<Address> = Vec::with_capacity(k);
            for j in 0..k {
                addresses.push(Address {
                    address: format!("{}.{}.{}.{}:{}", i + 1, i + 1, i + 1, i + 1, j).into(),
                    ..Default::default()
                });
            }
            endpoints.push(Endpoint {
                addresses,
                ..Default::default()
            })
        }
        endpoints
    }

    // Sends a resolver update to the LB policy with the specified endpoint.
    fn send_resolver_update_to_policy(
        lb_policy: &mut dyn LbPolicy,
        endpoints: Vec<Endpoint>,
        tcc: &mut dyn ChannelController,
    ) {
        let update = ResolverUpdate {
            endpoints: Ok(endpoints),
            ..Default::default()
        };
        let _ = lb_policy.resolver_update(update, None, tcc);
    }

    fn send_resolver_error_to_policy(
        lb_policy: &mut dyn LbPolicy,
        err: String,
        tcc: &mut dyn ChannelController,
    ) {
        let update = ResolverUpdate {
            endpoints: Err(err),
            ..Default::default()
        };
        let _ = lb_policy.resolver_update(update, None, tcc);
    }

    fn move_subchannel_to_state(
        lb_policy: &mut dyn LbPolicy,
        subchannel: Arc<dyn Subchannel>,
        tcc: &mut dyn ChannelController,
        state: ConnectivityState,
    ) {
        lb_policy.subchannel_update(
            subchannel,
            &SubchannelState {
                connectivity_state: state,
                ..Default::default()
            },
            tcc,
        );
    }

    fn move_subchannel_to_transient_failure(
        lb_policy: &mut dyn LbPolicy,
        subchannel: Arc<dyn Subchannel>,
        err: &str,
        tcc: &mut dyn ChannelController,
    ) {
        lb_policy.subchannel_update(
            subchannel.clone(),
            &SubchannelState {
                connectivity_state: ConnectivityState::TransientFailure,
                last_connection_error: Some(Arc::from(Box::from(err.to_owned()))),
            },
            tcc,
        );
    }

    struct TestOneSubchannelPicker {
        sc: Arc<dyn Subchannel>,
    }

    impl Picker for TestOneSubchannelPicker {
        fn pick(&self, request: &Request) -> PickResult {
            PickResult::Pick(Pick {
                subchannel: self.sc.clone(),
                on_complete: None,
                metadata: MetadataMap::new(),
            })
        }
    }

    fn address_list_from_endpoints(endpoints: &[Endpoint]) -> Vec<Address> {
        let mut addresses: Vec<Address> = endpoints
            .iter()
            .flat_map(|ep| ep.addresses.clone())
            .collect();
        let mut uniques = HashSet::new();
        addresses.retain(|e| uniques.insert(e.clone()));
        addresses
    }

    struct PickFirstState {
        subchannel_list: Option<TestSubchannelList>,
        selected_subchannel: Option<Arc<dyn Subchannel>>,
        addresses: Vec<Address>,
        connectivity_state: ConnectivityState,
    }

    // TODO: Replace with Pick First child once merged.
    // Defines the functions resolver_update and subchannel_update to test round
    // robin. Simple version of PickFirst. It just creates a subchannel and then
    // sends the appropriate picker update.
    fn create_funcs_for_roundrobin_tests() -> StubPolicyFuncs {
        StubPolicyFuncs {
            // Closure for resolver_update. It creates a subchannel for the
            // endpoint it receives and stores which endpoint it received and
            // which subchannel this child created in the data field.
            resolver_update: Some(
                move |data: &mut StubPolicyData, update: ResolverUpdate, _, channel_controller| {
                    let state = data
                        .test_data
                        .get_or_insert_with(|| {
                            Box::new(PickFirstState {
                                subchannel_list: None,
                                selected_subchannel: None,
                                addresses: vec![],
                                connectivity_state: ConnectivityState::Connecting,
                            })
                        })
                        .downcast_mut::<PickFirstState>()
                        .unwrap();

                    match update.endpoints {
                        Ok(endpoints) => {
                            let new_addresses = address_list_from_endpoints(&endpoints);

                            if new_addresses.is_empty() {
                                println!("new addresses is empty");
                                channel_controller.update_picker(LbState {
                                    connectivity_state: ConnectivityState::TransientFailure,
                                    picker: Arc::new(Failing {
                                        error: "received empty address list from the name resolver"
                                            .to_string(),
                                    }),
                                });
                                state.connectivity_state = ConnectivityState::TransientFailure;
                                channel_controller.request_resolution();
                                return Err(
                                    "received empty address list from the name resolver".into()
                                );
                            }

                            if state.connectivity_state != ConnectivityState::Idle {
                                state.subchannel_list = Some(TestSubchannelList::new(
                                    &new_addresses,
                                    channel_controller,
                                ));
                            }
                            state.addresses = new_addresses;
                        }
                        Err(error) => {
                            if state.addresses.is_empty()
                                || state.connectivity_state == ConnectivityState::TransientFailure
                            {
                                channel_controller.update_picker(LbState {
                                    connectivity_state: ConnectivityState::TransientFailure,
                                    picker: Arc::new(Failing {
                                        error: error.to_string(),
                                    }),
                                });
                                state.connectivity_state = ConnectivityState::TransientFailure;
                                channel_controller.request_resolution();
                            }
                        }
                    }
                    Ok(())
                },
            ),
            // Closure for subchannel_update. Verify that the subchannel that
            // being updated now is the same one that this child policy created
            // in resolver_update. It then sends a picker of the same state that
            // was passed to it.
            subchannel_update: Some(
                move |data: &mut StubPolicyData, subchannel, state, channel_controller| {
                    // Retrieve the specific TestState from the generic test_data field.
                    // This downcasts the `Any` trait object
                    if let Some(test_data) = data.test_data.as_mut() {
                        if let Some(test_state) = test_data.downcast_mut::<PickFirstState>() {
                            if let Some(scl) = &mut test_state.subchannel_list {
                                assert!(
                                scl.contains(&subchannel),
                                "subchannel_update received an update for a subchannel it does not own."
                                );
                                if scl.contains(&subchannel) {
                                    match state.connectivity_state {
                                        ConnectivityState::Ready => {
                                            channel_controller.update_picker(LbState {
                                                connectivity_state: state.connectivity_state,
                                                picker: Arc::new(TestOneSubchannelPicker {
                                                    sc: subchannel,
                                                }),
                                            });
                                            test_state.connectivity_state =
                                                ConnectivityState::Ready;
                                        }
                                        ConnectivityState::Idle => {}
                                        ConnectivityState::Connecting => {
                                            channel_controller.update_picker(LbState {
                                                connectivity_state: state.connectivity_state,
                                                picker: Arc::new(QueuingPicker {}),
                                            });
                                            test_state.connectivity_state =
                                                ConnectivityState::Connecting;
                                        }
                                        ConnectivityState::TransientFailure => {
                                            channel_controller.update_picker(LbState {
                                                connectivity_state: state.connectivity_state,
                                                picker: Arc::new(Failing {
                                                    error: state
                                                        .last_connection_error
                                                        .as_ref()
                                                        .unwrap()
                                                        .to_string(),
                                                }),
                                            });
                                            test_state.connectivity_state =
                                                ConnectivityState::TransientFailure;
                                        }
                                    }
                                }
                            }
                        }
                    }
                },
            ),
        }
    }

    // Creates a new endpoint with the specified number of addresses.
    fn create_endpoint_with_n_addresses(n: usize) -> Endpoint {
        let mut addresses = Vec::new();
        for i in 0..n {
            addresses.push(Address {
                address: format!("{}.{}.{}.{}:{}", i, i, i, i, i).into(),
                ..Default::default()
            });
        }
        Endpoint {
            addresses,
            ..Default::default()
        }
    }

    // Verifies that the expected number of subchannels is created. Returns the
    // subchannels created.
    async fn verify_subchannel_creation_from_policy(
        rx_events: &mut mpsc::UnboundedReceiver<TestEvent>,
        number_of_subchannels: usize,
    ) -> Vec<Arc<dyn Subchannel>> {
        let mut subchannels = Vec::new();
        for _ in 0..number_of_subchannels {
            match rx_events.recv().await.unwrap() {
                TestEvent::NewSubchannel(sc) => {
                    subchannels.push(sc);
                }
                other => panic!("unexpected event {:?}", other),
            };
        }
        subchannels
    }

    // Verifies that the channel moves to CONNECTING state with a queuing picker.
    //
    // Returns the picker for tests to make more picks, if required.
    async fn verify_connecting_picker_from_policy(
        rx_events: &mut mpsc::UnboundedReceiver<TestEvent>,
    ) -> Arc<dyn Picker> {
        println!("verify connecting picker");
        match rx_events.recv().await.unwrap() {
            TestEvent::UpdatePicker(update) => {
                println!("connectivity state is {}", update.connectivity_state);
                assert!(update.connectivity_state == ConnectivityState::Connecting);
                let req = test_utils::new_request();
                assert!(update.picker.pick(&req) == PickResult::Queue);
                return update.picker.clone();
            }
            other => panic!("unexpected event {:?}", other),
        }
    }

    // Verifies that the channel moves to READY state with a picker that returns
    // the given subchannel.
    //
    // Returns the picker for tests to make more picks, if required.
    async fn verify_ready_picker_from_policy(
        rx_events: &mut mpsc::UnboundedReceiver<TestEvent>,
        subchannel: Arc<dyn Subchannel>,
    ) -> Arc<dyn Picker> {
        println!("verify ready picker");
        match rx_events.recv().await.unwrap() {
            TestEvent::UpdatePicker(update) => {
                println!(
                    "connectivity state for ready picker is {}",
                    update.connectivity_state
                );
                assert!(update.connectivity_state == ConnectivityState::Ready);
                let req = test_utils::new_request();
                match update.picker.pick(&req) {
                    PickResult::Pick(pick) => {
                        println!("selected subchannel is {}", pick.subchannel);
                        println!("should've been selected subchannel is {}", subchannel);
                        assert!(pick.subchannel == subchannel.clone());
                        update.picker.clone()
                    }
                    other => panic!("unexpected pick result {}", other),
                }
            }
            other => panic!("unexpected event {:?}", other),
        }
    }

    // Returns the picker for when there are multiple pickers in the ready
    // picker.
    async fn verify_roundrobin_ready_picker_from_policy(
        rx_events: &mut mpsc::UnboundedReceiver<TestEvent>,
    ) -> Arc<dyn Picker> {
        println!("verify ready picker");
        match rx_events.recv().await.unwrap() {
            TestEvent::UpdatePicker(update) => {
                println!(
                    "connectivity state for ready picker is {}",
                    update.connectivity_state
                );
                assert!(update.connectivity_state == ConnectivityState::Ready);
                let req = test_utils::new_request();
                match update.picker.pick(&req) {
                    PickResult::Pick(pick) => update.picker.clone(),
                    other => panic!("unexpected pick result {}", other),
                }
            }
            other => panic!("unexpected event {:?}", other),
        }
    }

    // Verifies that the channel moves to TRANSIENT_FAILURE state with a picker
    // that returns an error with the given message. The error code should be
    // UNAVAILABLE..
    //
    // Returns the picker for tests to make more picks, if required.
    async fn verify_transient_failure_picker_from_policy(
        rx_events: &mut mpsc::UnboundedReceiver<TestEvent>,
        want_error: String,
    ) -> Arc<dyn Picker> {
        let picker = match rx_events.recv().await.unwrap() {
            TestEvent::UpdatePicker(update) => {
                assert!(update.connectivity_state == ConnectivityState::TransientFailure);
                let req = test_utils::new_request();
                match update.picker.pick(&req) {
                    PickResult::Fail(status) => {
                        assert!(status.code() == tonic::Code::Unavailable);
                        assert!(status.message().contains(&want_error));
                        update.picker.clone()
                    }
                    other => panic!("unexpected pick result {}", other),
                }
            }
            other => panic!("unexpected event {:?}", other),
        };
        picker
    }

    // Verifies that the LB policy requests re-resolution.
    async fn verify_resolution_request(rx_events: &mut mpsc::UnboundedReceiver<TestEvent>) {
        println!("verifying resolution request");
        match rx_events.recv().await.unwrap() {
            TestEvent::RequestResolution => {}
            other => panic!("unexpected event {:?}", other),
        };
    }

    const DEFAULT_TEST_SHORT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(100);

    async fn verify_no_activity_from_policy(rx_events: &mut mpsc::UnboundedReceiver<TestEvent>) {
        tokio::select! {
            _ = tokio::time::sleep(DEFAULT_TEST_SHORT_TIMEOUT) => {}
            event = rx_events.recv() => {
                panic!("unexpected event {:?}", event.unwrap());
            }
        }
    }

    #[test]
    fn roundrobin_builder_name() -> Result<(), String> {
        round_robin::reg();

        let builder: Arc<dyn LbPolicyBuilder> = match GLOBAL_LB_REGISTRY.get_policy("round_robin") {
            Some(b) => b,
            None => {
                return Err(String::from("round_robin LB policy not registered"));
            }
        };
        assert_eq!(builder.name(), "round_robin");
        Ok(())
    }

    // Tests the scenario where the resolver returns an error before a valid
    // update. The LB policy should move to TRANSIENT_FAILURE state with a
    // failing picker.
    #[tokio::test]
    async fn roundrobin_resolver_error_before_a_valid_update() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup(
            create_funcs_for_roundrobin_tests(),
            "stub-roundrobin_resolver_error_before_a_valid_update",
        );
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();
        let resolver_error = String::from("resolver error");
        send_resolver_error_to_policy(lb_policy, resolver_error.clone(), tcc);
        verify_transient_failure_picker_from_policy(&mut rx_events, resolver_error).await;
    }

    // Tests the scenario where the resolver returns an error after a valid update
    // and the LB policy has moved to READY. The LB policy should ignore the error
    // and continue using the previously received update.
    #[tokio::test]
    async fn roundrobin_resolver_error_after_a_valid_update_in_ready() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup(
            create_funcs_for_roundrobin_tests(),
            "stub-roundrobin_resolver_error_after_a_valid_update_in_ready",
        );
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();
        let endpoint = create_endpoint_with_n_addresses(1);
        send_resolver_update_to_policy(lb_policy, vec![endpoint.clone()], tcc);
        let subchannels = verify_subchannel_creation_from_policy(&mut rx_events, 1).await;

        move_subchannel_to_state(
            lb_policy,
            subchannels[0].clone(),
            tcc,
            ConnectivityState::Connecting,
        );
        verify_connecting_picker_from_policy(&mut rx_events).await;

        move_subchannel_to_state(
            lb_policy,
            subchannels[0].clone(),
            tcc,
            ConnectivityState::Ready,
        );
        let picker = verify_ready_picker_from_policy(&mut rx_events, subchannels[0].clone()).await;
        let resolver_error = String::from("resolver error");
        send_resolver_error_to_policy(lb_policy, resolver_error.clone(), tcc);
        verify_no_activity_from_policy(&mut rx_events).await;

        let req = test_utils::new_request();
        match picker.pick(&req) {
            PickResult::Pick(pick) => {
                assert!(pick.subchannel == subchannels[0].clone());
            }
            other => panic!("unexpected pick result {}", other),
        }
    }

    // Tests the scenario where the resolver returns an error after a valid update
    // and the LB policy is still trying to connect. The LB policy should ignore the
    // error and continue using the previously received update.
    #[tokio::test]
    async fn roundrobin_resolver_error_after_a_valid_update_in_connecting() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup(
            create_funcs_for_roundrobin_tests(),
            "stub-roundrobin_resolver_error_after_a_valid_update_in_connecting",
        );
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let endpoint = create_endpoint_with_n_addresses(2);
        send_resolver_update_to_policy(lb_policy, vec![endpoint.clone()], tcc);
        let subchannels = verify_subchannel_creation_from_policy(&mut rx_events, 2).await;

        move_subchannel_to_state(
            lb_policy,
            subchannels[0].clone(),
            tcc,
            ConnectivityState::Connecting,
        );
        let picker = verify_connecting_picker_from_policy(&mut rx_events).await;

        let resolver_error = String::from("resolver error");

        send_resolver_error_to_policy(lb_policy, resolver_error.clone(), tcc);

        verify_no_activity_from_policy(&mut rx_events).await;

        let req = test_utils::new_request();
        match picker.pick(&req) {
            PickResult::Queue => {}
            other => panic!("unexpected pick result {}", other),
        }
    }

    // Tests the scenario where the resolver returns an error after a valid update
    // and the LB policy has moved to TRANSIENT_FAILURE after attempting to connect
    // to all addresses.  The LB policy should send a new picker that returns the
    // error from the resolver.
    #[tokio::test]
    async fn roundrobin_resolver_error_after_a_valid_update_in_tf() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup(
            create_funcs_for_roundrobin_tests(),
            "stub-roundrobin_resolver_error_after_a_valid_update_in_tf",
        );
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();
        let endpoint = create_endpoint_with_n_addresses(1);
        send_resolver_update_to_policy(lb_policy, vec![endpoint.clone()], tcc);
        let subchannels = verify_subchannel_creation_from_policy(&mut rx_events, 1).await;
        move_subchannel_to_state(
            lb_policy,
            subchannels[0].clone(),
            tcc,
            ConnectivityState::Connecting,
        );
        verify_connecting_picker_from_policy(&mut rx_events).await;
        let connection_error = String::from("test connection error");
        move_subchannel_to_transient_failure(
            lb_policy,
            subchannels[0].clone(),
            &connection_error,
            tcc,
        );
        verify_transient_failure_picker_from_policy(&mut rx_events, connection_error).await;
        let resolver_error = String::from("resolver error");
        send_resolver_error_to_policy(lb_policy, resolver_error.clone(), tcc);
        verify_resolution_request(&mut rx_events).await;
        verify_transient_failure_picker_from_policy(&mut rx_events, resolver_error).await;
    }

    #[tokio::test]
    async fn roundrobin_simple_test() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup(
            create_funcs_for_roundrobin_tests(),
            "stub-roundrobin_simple_test",
        );
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let endpoint = create_endpoint_with_n_addresses(2);
        send_resolver_update_to_policy(lb_policy, vec![endpoint.clone()], tcc);
        let subchannels = verify_subchannel_creation_from_policy(&mut rx_events, 2).await;
        move_subchannel_to_state(
            lb_policy,
            subchannels[0].clone(),
            tcc,
            ConnectivityState::Connecting,
        );
        verify_connecting_picker_from_policy(&mut rx_events).await;
        move_subchannel_to_state(
            lb_policy,
            subchannels[1].clone(),
            tcc,
            ConnectivityState::Connecting,
        );
        verify_connecting_picker_from_policy(&mut rx_events).await;
        move_subchannel_to_state(
            lb_policy,
            subchannels[0].clone(),
            tcc,
            ConnectivityState::Ready,
        );
        verify_ready_picker_from_policy(&mut rx_events, subchannels[0].clone()).await;
    }

    // Round Robin should round robin across endpoints.
    #[tokio::test]
    async fn roundrobin_picks_are_round_robin() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup(
            create_funcs_for_roundrobin_tests(),
            "stub-roundrobin_picks_are_round_robin",
        );
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let endpoints = create_n_endpoints_with_k_addresses(2, 1);
        send_resolver_update_to_policy(lb_policy, endpoints.clone(), tcc);

        let subchannels = verify_subchannel_creation_from_policy(&mut rx_events, 1).await;
        let second_subchannels = verify_subchannel_creation_from_policy(&mut rx_events, 1).await;
        let mut all_subchannels = subchannels.clone();
        all_subchannels.extend(second_subchannels.clone());

        move_subchannel_to_state(
            lb_policy,
            all_subchannels[0].clone(),
            tcc,
            ConnectivityState::Connecting,
        );
        verify_connecting_picker_from_policy(&mut rx_events).await;
        move_subchannel_to_state(
            lb_policy,
            all_subchannels[0].clone(),
            tcc,
            ConnectivityState::Ready,
        );
        verify_ready_picker_from_policy(&mut rx_events, all_subchannels[0].clone()).await;
        move_subchannel_to_state(
            lb_policy,
            all_subchannels[1].clone(),
            tcc,
            ConnectivityState::Ready,
        );
        let picker = verify_roundrobin_ready_picker_from_policy(&mut rx_events).await;
        let req = test_utils::new_request();
        let mut picked = Vec::new();
        for _ in 0..4 {
            match picker.pick(&req) {
                PickResult::Pick(pick) => {
                    println!("picked subchannel is {}", pick.subchannel);
                    picked.push(pick.subchannel.clone())
                }
                other => panic!("unexpected pick result {}", other),
            }
        }
        assert!(
            picked[0] != picked[1].clone(),
            "Should alternate between subchannels"
        );
        assert_eq!(&picked[0], &picked[2]);
        assert_eq!(&picked[1], &picked[3]);
        assert!(picked.contains(&subchannels[0]));
        assert!(picked.contains(&second_subchannels[0]));
    }

    // If round robin receives no addresses in a resolver update,
    // it should go into transient failure.
    #[tokio::test]
    async fn roundrobin_addresses_removed() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup(
            create_funcs_for_roundrobin_tests(),
            "stub-roundrobin_addresses_removed",
        );
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let endpoints = create_n_endpoints_with_k_addresses(2, 1);
        send_resolver_update_to_policy(lb_policy, endpoints, tcc);

        let subchannels = verify_subchannel_creation_from_policy(&mut rx_events, 1).await;
        let second_subchannels = verify_subchannel_creation_from_policy(&mut rx_events, 1).await;

        let mut all_subchannels = subchannels.clone();
        all_subchannels.extend(second_subchannels.clone());
        move_subchannel_to_state(
            lb_policy,
            subchannels[0].clone(),
            tcc,
            ConnectivityState::Connecting,
        );
        verify_connecting_picker_from_policy(&mut rx_events).await;
        move_subchannel_to_state(
            lb_policy,
            second_subchannels[0].clone(),
            tcc,
            ConnectivityState::Connecting,
        );
        verify_connecting_picker_from_policy(&mut rx_events).await;
        let update = ResolverUpdate {
            endpoints: Ok(vec![]),
            ..Default::default()
        };
        let _ = lb_policy.resolver_update(update, None, tcc);
        let want_error = "received empty address list from the name resolver";
        verify_resolution_request(&mut rx_events).await;
        verify_resolution_request(&mut rx_events).await;
        verify_transient_failure_picker_from_policy(&mut rx_events, want_error.to_string()).await;
    }

    // Round robin should only round robin across children that are ready.
    // If a child leaves the ready state, Round Robin should only
    // pick from the children that are still Ready.
    #[tokio::test]
    async fn roundrobin_one_endpoint_down() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup(
            create_funcs_for_roundrobin_tests(),
            "stub-roundrobin_one_endpoint_down",
        );
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let endpoints = create_n_endpoints_with_k_addresses(2, 1);
        send_resolver_update_to_policy(lb_policy, endpoints.clone(), tcc);

        let subchannels = verify_subchannel_creation_from_policy(&mut rx_events, 1).await;

        let second_subchannels = verify_subchannel_creation_from_policy(&mut rx_events, 1).await;
        let mut all_subchannels = subchannels.clone();
        all_subchannels.extend(second_subchannels.clone());

        move_subchannel_to_state(
            lb_policy,
            all_subchannels[0].clone(),
            tcc,
            ConnectivityState::Connecting,
        );
        verify_connecting_picker_from_policy(&mut rx_events).await;
        move_subchannel_to_state(
            lb_policy,
            all_subchannels[0].clone(),
            tcc,
            ConnectivityState::Ready,
        );
        let picker =
            verify_ready_picker_from_policy(&mut rx_events, all_subchannels[0].clone()).await;
        move_subchannel_to_state(
            lb_policy,
            all_subchannels[1].clone(),
            tcc,
            ConnectivityState::Ready,
        );
        let picker = verify_roundrobin_ready_picker_from_policy(&mut rx_events).await;
        let req = test_utils::new_request();
        let mut picked = Vec::new();
        for _ in 0..4 {
            match picker.pick(&req) {
                PickResult::Pick(pick) => {
                    println!("picked subchannel is {}", pick.subchannel);
                    picked.push(pick.subchannel.clone())
                }
                other => panic!("unexpected pick result {}", other),
            }
        }
        assert!(
            picked[0] != picked[1].clone(),
            "Should alternate between subchannels"
        );
        assert_eq!(&picked[0], &picked[2]);
        assert_eq!(&picked[1], &picked[3]);

        assert!(picked.contains(&subchannels[0]));
        assert!(picked.contains(&second_subchannels[0]));
        let subchannel_being_removed = all_subchannels[1].clone();
        let error = "endpoint down";
        move_subchannel_to_transient_failure(lb_policy, all_subchannels[1].clone(), error, tcc);

        let new_picker = verify_roundrobin_ready_picker_from_policy(&mut rx_events).await;

        let req = test_utils::new_request();
        let mut picked = Vec::new();
        for _ in 0..4 {
            match new_picker.pick(&req) {
                PickResult::Pick(pick) => {
                    println!("picked subchannel is {}", pick.subchannel);
                    picked.push(pick.subchannel.clone())
                }
                other => panic!("unexpected pick result {}", other),
            }
        }

        assert_eq!(&picked[0], &picked[2]);
        assert_eq!(&picked[1], &picked[3]);
        assert!(picked.contains(&subchannels[0]));
        assert!(!picked.contains(&subchannel_being_removed));
    }

    // If Round Robin receives a resolver update that removes an endpoint and
    // adds a new endpoint from a previous update, that endpoint's subchannels
    // should not be apart of its picks anymore and should be removed. It should
    // then roundrobin across the endpoints it still has and the new one.
    #[tokio::test]
    async fn roundrobin_pick_after_resolved_updated_hosts() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup(
            create_funcs_for_roundrobin_tests(),
            "stub-roundrobin_pick_after_resolved_updated_hosts",
        );
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();
        let removed_addr = Address {
            address: "removed".to_string().into(),
            ..Default::default()
        };
        let old_addr = Address {
            address: "old".to_string().into(),
            ..Default::default()
        };
        let removed_endpoint = Endpoint {
            addresses: vec![removed_addr.clone()],
            ..Default::default()
        };
        let old_endpoint = Endpoint {
            addresses: vec![old_addr.clone()],
            ..Default::default()
        };

        send_resolver_update_to_policy(
            lb_policy,
            vec![removed_endpoint.clone(), old_endpoint.clone()],
            tcc,
        );

        let mut all_addresses = removed_endpoint.addresses.clone();
        all_addresses.extend(old_endpoint.addresses.clone());
        let all_subchannels = verify_subchannel_creation_from_policy(&mut rx_events, 2).await;
        let removed_sc = all_subchannels
            .iter()
            .find(|sc| sc.address().address == "removed".to_string().into())
            .unwrap()
            .clone();
        let old_sc = all_subchannels
            .iter()
            .find(|sc| sc.address().address == "old".to_string().into())
            .unwrap()
            .clone();
        println!("removed_subchannels[0] address: {}", removed_sc.address());
        println!("old_subchannels[0] address: {}", old_sc.address());

        move_subchannel_to_state(
            lb_policy,
            removed_sc.clone(),
            tcc,
            ConnectivityState::Connecting,
        );
        verify_connecting_picker_from_policy(&mut rx_events).await;
        move_subchannel_to_state(
            lb_policy,
            old_sc.clone(),
            tcc,
            ConnectivityState::Connecting,
        );
        verify_connecting_picker_from_policy(&mut rx_events).await;

        move_subchannel_to_state(lb_policy, removed_sc.clone(), tcc, ConnectivityState::Ready);
        verify_ready_picker_from_policy(&mut rx_events, removed_sc.clone()).await;
        move_subchannel_to_state(lb_policy, old_sc.clone(), tcc, ConnectivityState::Ready);
        let picker = verify_roundrobin_ready_picker_from_policy(&mut rx_events).await;
        let req = test_utils::new_request();
        let mut picked = Vec::new();
        for _ in 0..4 {
            match picker.pick(&req) {
                PickResult::Pick(pick) => {
                    println!("picker subchannel is {}", pick.subchannel.clone());
                    picked.push(pick.subchannel.clone())
                }
                other => panic!("unexpected pick result {}", other),
            }
        }
        assert!(picked.contains(&removed_sc));
        assert!(picked.contains(&old_sc));
        let new_addr = Address {
            address: "new".to_string().into(),
            ..Default::default()
        };
        let new_endpoint = Endpoint {
            addresses: vec![new_addr.clone()],
            ..Default::default()
        };
        let mut all_new_addresses = old_endpoint.addresses.clone();
        all_new_addresses.extend(new_endpoint.addresses.clone());

        send_resolver_update_to_policy(
            lb_policy,
            vec![old_endpoint.clone(), new_endpoint.clone()],
            tcc,
        );

        let new_subchannels = verify_subchannel_creation_from_policy(&mut rx_events, 2).await;
        let new_sc = new_subchannels
            .iter()
            .find(|sc| sc.address().address == "new".to_string().into())
            .unwrap()
            .clone();
        let old_sc = new_subchannels
            .iter()
            .find(|sc| sc.address().address == "new".to_string().into())
            .unwrap()
            .clone();

        // Now proceed to drive states on `old_sc` (from before) and `new_sc` (just created)
        move_subchannel_to_state(lb_policy, old_sc.clone(), tcc, ConnectivityState::Ready);
        let _ = verify_roundrobin_ready_picker_from_policy(&mut rx_events).await;

        move_subchannel_to_state(
            lb_policy,
            new_sc.clone(),
            tcc,
            ConnectivityState::Connecting,
        );
        let _ = verify_roundrobin_ready_picker_from_policy(&mut rx_events).await;

        move_subchannel_to_state(lb_policy, new_sc.clone(), tcc, ConnectivityState::Ready);
        let new_picker = verify_roundrobin_ready_picker_from_policy(&mut rx_events).await;

        // Picks should now be only from old+new and exclude removed
        let req = test_utils::new_request();
        let mut picked = Vec::new();
        for _ in 0..4 {
            match new_picker.pick(&req) {
                PickResult::Pick(pick) => picked.push(pick.subchannel.clone()),
                other => panic!("unexpected pick result {}", other),
            }
        }
        assert!(picked.contains(&old_sc));
        assert!(picked.contains(&new_sc));
        assert!(!picked.contains(&removed_sc));
    }

    #[tokio::test]
    async fn roundrobin_zero_addresses_from_resolver_before_valid_update() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup(
            create_funcs_for_roundrobin_tests(),
            "stub-roundrobin_zero_addresses_from_resolver_before_valid_update",
        );
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let endpoints = create_n_endpoints_with_k_addresses(4, 0);
        let update = ResolverUpdate {
            endpoints: Ok(endpoints.clone()),
            ..Default::default()
        };
        let subchannels = verify_subchannel_creation_from_policy(&mut rx_events, 0).await;
        // verify_transient_failure_picker_from_policy(
        //     &mut rx_events,
        //     "received empty address list from the name resolver".to_string(),
        // )
        // .await;
    }

    // Round robin should stay in transient failure until a child reports ready
    #[tokio::test]
    async fn roundrobin_stay_transient_failure_until_ready() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup(
            create_funcs_for_roundrobin_tests(),
            "stub-roundrobin_stay_transient_failure_until_ready",
        );
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();
        let endpoints = create_n_endpoints_with_k_addresses(2, 1);
        send_resolver_update_to_policy(lb_policy, endpoints.clone(), tcc);
        let subchannels = verify_subchannel_creation_from_policy(&mut rx_events, 1).await;
        let second_subchannels = verify_subchannel_creation_from_policy(&mut rx_events, 1).await;
        move_subchannel_to_state(
            lb_policy,
            subchannels[0].clone(),
            tcc,
            ConnectivityState::Connecting,
        );
        verify_connecting_picker_from_policy(&mut rx_events).await;
        move_subchannel_to_state(
            lb_policy,
            second_subchannels[0].clone(),
            tcc,
            ConnectivityState::Connecting,
        );
        verify_connecting_picker_from_policy(&mut rx_events).await;
        let first_error = String::from("test connection error 1");
        move_subchannel_to_transient_failure(lb_policy, subchannels[0].clone(), &first_error, tcc);
        verify_connecting_picker_from_policy(&mut rx_events).await;
        move_subchannel_to_transient_failure(
            lb_policy,
            second_subchannels[0].clone(),
            &first_error,
            tcc,
        );
        verify_transient_failure_picker_from_policy(&mut rx_events, first_error).await;
        move_subchannel_to_state(
            lb_policy,
            subchannels[0].clone(),
            tcc,
            ConnectivityState::Ready,
        );
        verify_ready_picker_from_policy(&mut rx_events, subchannels[0].clone()).await;
    }

    // Tests the scenario where the resolver returns an update with no endpoints
    // (before sending any valid update). The LB policy should move to
    // TRANSIENT_FAILURE state with a failing picker.
    #[tokio::test]
    async fn roundrobin_zero_endpoints_from_resolver_before_valid_update() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup(
            create_funcs_for_roundrobin_tests(),
            "stub-roundrobin_zero_endpoints_from_resolver_before_valid_update",
        );
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();
        send_resolver_update_to_policy(lb_policy, vec![], tcc);
        verify_transient_failure_picker_from_policy(
            &mut rx_events,
            "received empty address list from the name resolver".to_string(),
        )
        .await;
    }

    // Tests the scenario where the resolver returns an update with no endpoints
    // after sending a valid update (and the LB policy has moved to READY). The LB
    // policy should move to TRANSIENT_FAILURE state with a failing picker.
    #[tokio::test]
    async fn roundrobin_zero_endpoints_from_resolver_after_valid_update() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup(
            create_funcs_for_roundrobin_tests(),
            "stub-roundrobin_zero_endpoints_from_resolver_after_valid_update",
        );
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let endpoint = create_endpoint_with_n_addresses(2);
        send_resolver_update_to_policy(lb_policy, vec![endpoint.clone()], tcc);
        let subchannels = verify_subchannel_creation_from_policy(&mut rx_events, 2).await;

        move_subchannel_to_state(
            lb_policy,
            subchannels[0].clone(),
            tcc,
            ConnectivityState::Connecting,
        );
        verify_connecting_picker_from_policy(&mut rx_events).await;

        move_subchannel_to_state(
            lb_policy,
            subchannels[0].clone(),
            tcc,
            ConnectivityState::Ready,
        );

        verify_ready_picker_from_policy(&mut rx_events, subchannels[0].clone()).await;

        let update = ResolverUpdate {
            endpoints: Ok(vec![]),
            ..Default::default()
        };
        assert!(lb_policy.resolver_update(update, None, tcc).is_err());
        verify_resolution_request(&mut rx_events).await;
        verify_transient_failure_picker_from_policy(
            &mut rx_events,
            "received empty address list from the name resolver".to_string(),
        )
        .await;
    }

    // Tests the scenario where the resolver returns an update with multiple
    // address. The LB policy should create subchannels for all address, and attempt
    // to connect to them in order, until a connection succeeds, at which point it
    // should move to READY state with a picker that returns that subchannel.
    #[tokio::test]
    async fn roundrobin_with_multiple_backends_first_backend_is_ready() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup(
            create_funcs_for_roundrobin_tests(),
            "stub-roundrobin_with_multiple_backends_first_backend_is_ready",
        );
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let endpoint = create_endpoint_with_n_addresses(2);
        send_resolver_update_to_policy(lb_policy, vec![endpoint.clone()], tcc);
        let subchannels = verify_subchannel_creation_from_policy(&mut rx_events, 2).await;

        move_subchannel_to_state(
            lb_policy,
            subchannels[0].clone(),
            tcc,
            ConnectivityState::Connecting,
        );
        verify_connecting_picker_from_policy(&mut rx_events).await;
        move_subchannel_to_state(
            lb_policy,
            subchannels[0].clone(),
            tcc,
            ConnectivityState::Ready,
        );

        verify_ready_picker_from_policy(&mut rx_events, subchannels[0].clone()).await;
    }

    // Tests the scenario where the resolver returns an update with multiple
    // addresses and the LB policy successfully connects to first one and moves to
    // READY. The resolver then returns an update with a new address list that
    // contains the address of the currently connected subchannel. The LB policy
    // should create subchannels for the new addresses, and then see that the
    // currently connected subchannel is in the new address list. It should then
    // send a new READY picker that returns the currently connected subchannel.
    #[tokio::test]
    async fn roundrobin_resolver_update_contains_currently_ready_subchannel() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup(
            create_funcs_for_roundrobin_tests(),
            "stub-roundrobin_resolver_update_contains_currently_ready_subchannel",
        );
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let endpoints = create_endpoint_with_n_addresses(2);
        send_resolver_update_to_policy(lb_policy, vec![endpoints.clone()], tcc);
        let subchannels = verify_subchannel_creation_from_policy(&mut rx_events, 2).await;
        move_subchannel_to_state(
            lb_policy,
            subchannels[0].clone(),
            tcc,
            ConnectivityState::Connecting,
        );
        verify_connecting_picker_from_policy(&mut rx_events).await;
        move_subchannel_to_state(
            lb_policy,
            subchannels[0].clone(),
            tcc,
            ConnectivityState::Ready,
        );
        verify_ready_picker_from_policy(&mut rx_events, subchannels[0].clone()).await;

        let mut endpoints = create_endpoint_with_n_addresses(4);
        endpoints.addresses.reverse();
        send_resolver_update_to_policy(lb_policy, vec![endpoints.clone()], tcc);
        let subchannels = verify_subchannel_creation_from_policy(&mut rx_events, 4).await;
        // TODO(easwars): Once Pick First gets merged, this won't send a
        // CONNECTING picker.
        lb_policy.subchannel_update(subchannels[0].clone(), &SubchannelState::default(), tcc);
        lb_policy.subchannel_update(subchannels[1].clone(), &SubchannelState::default(), tcc);
        lb_policy.subchannel_update(subchannels[2].clone(), &SubchannelState::default(), tcc);
        lb_policy.subchannel_update(
            subchannels[3].clone(),
            &SubchannelState {
                connectivity_state: ConnectivityState::Ready,
                ..Default::default()
            },
            tcc,
        );
        verify_ready_picker_from_policy(&mut rx_events, subchannels[3].clone()).await;
    }
}
