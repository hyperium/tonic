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

use std::error::Error;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Once;

use crate::client::load_balancing::child_manager::ChildManager;
use crate::client::load_balancing::child_manager::ChildUpdate;
use crate::client::load_balancing::pick_first;
use crate::client::load_balancing::ChannelController;
use crate::client::load_balancing::FailingPicker;
use crate::client::load_balancing::LbConfig;
use crate::client::load_balancing::LbPolicy;
use crate::client::load_balancing::LbPolicyBuilder;
use crate::client::load_balancing::LbPolicyOptions;
use crate::client::load_balancing::LbState;
use crate::client::load_balancing::PickResult;
use crate::client::load_balancing::Picker;
use crate::client::load_balancing::Subchannel;
use crate::client::load_balancing::SubchannelState;
use crate::client::load_balancing::GLOBAL_LB_REGISTRY;
use crate::client::name_resolution::Endpoint;
use crate::client::name_resolution::ResolverUpdate;
use crate::client::ConnectivityState;
use crate::service::Request;

pub(crate) static POLICY_NAME: &str = "round_robin";
static START: Once = Once::new();

#[derive(Debug)]
struct RoundRobinBuilder {}

impl LbPolicyBuilder for RoundRobinBuilder {
    fn build(&self, options: LbPolicyOptions) -> Box<dyn LbPolicy> {
        let child_manager = ChildManager::new(options.runtime, options.work_scheduler);
        Box::new(RoundRobinPolicy::new(
            child_manager,
            GLOBAL_LB_REGISTRY
                .get_policy(pick_first::POLICY_NAME)
                .unwrap(),
        ))
    }

    fn name(&self) -> &'static str {
        POLICY_NAME
    }
}

#[derive(Debug)]
struct RoundRobinPolicy {
    child_manager: ChildManager<Endpoint>,
    pick_first_builder: Arc<dyn LbPolicyBuilder>,
}

impl RoundRobinPolicy {
    fn new(
        child_manager: ChildManager<Endpoint>,
        pick_first_builder: Arc<dyn LbPolicyBuilder>,
    ) -> Self {
        Self {
            child_manager,
            pick_first_builder,
        }
    }

    // Sets the policy's state to TRANSIENT_FAILURE with a picker returning the
    // error string provided, then requests re-resolution from the channel.
    fn move_to_transient_failure(
        &mut self,
        error: String,
        channel_controller: &mut dyn ChannelController,
    ) {
        channel_controller.update_picker(LbState {
            connectivity_state: ConnectivityState::TransientFailure,
            picker: Arc::new(FailingPicker { error }),
        });
        channel_controller.request_resolution();
    }

    // Sends an aggregate picker based on states of children.
    //
    // The state is determined according to normal state aggregation rules, and
    // the picker round-robins between all children in that state.
    fn update_picker(&mut self, channel_controller: &mut dyn ChannelController) {
        if !self.child_manager.child_updated() {
            return;
        }
        let aggregate_state = self.child_manager.aggregate_states();
        let pickers = self
            .child_manager
            .children()
            .filter(|cs| cs.state.connectivity_state == aggregate_state)
            .map(|cs| cs.state.picker.clone())
            .collect();
        let picker_update = LbState {
            connectivity_state: aggregate_state,
            picker: Arc::new(RoundRobinPicker::new(pickers)),
        };
        channel_controller.update_picker(picker_update);
    }

    // Responds to an incoming ResolverUpdate containing an Err in endpoints by
    // forwarding it to all children unconditionally.  Updates the picker as
    // needed.
    fn handle_resolver_error(
        &mut self,
        resolver_update: ResolverUpdate,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let err = format!(
            "Received error from name resolver: {}",
            resolver_update.endpoints.as_ref().unwrap_err()
        );
        if self.child_manager.children().next().is_none() {
            // We had no children so we must produce an erroring picker.
            self.move_to_transient_failure(err.clone(), channel_controller);
            return Err(err.into());
        }
        // Forward the error to each child, ignoring their responses.
        let _ = self
            .child_manager
            .resolver_update(resolver_update, None, channel_controller);
        self.update_picker(channel_controller);
        Err(err.into())
    }
}

impl LbPolicy for RoundRobinPolicy {
    fn resolver_update(
        &mut self,
        update: ResolverUpdate,
        config: Option<&LbConfig>,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        if update.endpoints.is_err() {
            return self.handle_resolver_error(update, channel_controller);
        }

        // Shard the update by endpoint.
        let updates = update.endpoints.as_ref().unwrap().iter().map(|e| {
            let update = ResolverUpdate {
                attributes: crate::attributes::Attributes,
                endpoints: Ok(vec![e.clone()]),
                service_config: update.service_config.clone(),
                resolution_note: None,
            };
            ChildUpdate {
                child_identifier: e.clone(),
                child_policy_builder: self.pick_first_builder.clone(),
                child_update: Some((update, config.cloned())),
            }
        });
        self.child_manager
            .update(updates, channel_controller)
            .unwrap();

        if self.child_manager.children().next().is_none() {
            // There are no children remaining, so report this error and produce
            // an erroring picker.
            let err = "Received empty address list from the name resolver";
            self.move_to_transient_failure(err.into(), channel_controller);
            return Err(err.into());
        }

        self.update_picker(channel_controller);
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
        self.update_picker(channel_controller);
    }

    fn work(&mut self, channel_controller: &mut dyn ChannelController) {
        self.child_manager.work(channel_controller);
        self.update_picker(channel_controller);
    }

    fn exit_idle(&mut self, channel_controller: &mut dyn ChannelController) {
        self.child_manager.exit_idle(channel_controller);
        self.update_picker(channel_controller);
    }
}

/// Register round robin as a LbPolicy.
pub(crate) fn reg() {
    START.call_once(|| {
        GLOBAL_LB_REGISTRY.add_builder(RoundRobinBuilder {});
    });
}

#[derive(Debug)]
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
    use crate::client::load_balancing::child_manager::ChildManager;
    use crate::client::load_balancing::pick_first;
    use crate::client::load_balancing::round_robin::RoundRobinPolicy;
    use crate::client::load_balancing::round_robin::{self};
    use crate::client::load_balancing::test_utils::StubPolicyData;
    use crate::client::load_balancing::test_utils::StubPolicyFuncs;
    use crate::client::load_balancing::test_utils::TestChannelController;
    use crate::client::load_balancing::test_utils::TestEvent;
    use crate::client::load_balancing::test_utils::TestWorkScheduler;
    use crate::client::load_balancing::test_utils::{self};
    use crate::client::load_balancing::ChannelController;
    use crate::client::load_balancing::FailingPicker;
    use crate::client::load_balancing::LbPolicy;
    use crate::client::load_balancing::LbState;
    use crate::client::load_balancing::Pick;
    use crate::client::load_balancing::PickResult;
    use crate::client::load_balancing::Picker;
    use crate::client::load_balancing::QueuingPicker;
    use crate::client::load_balancing::Subchannel;
    use crate::client::load_balancing::SubchannelState;
    use crate::client::load_balancing::GLOBAL_LB_REGISTRY;
    use crate::client::name_resolution::Address;
    use crate::client::name_resolution::Endpoint;
    use crate::client::name_resolution::ResolverUpdate;
    use crate::client::ConnectivityState;
    use crate::rt::default_runtime;
    use crate::service::Request;
    use std::collections::HashSet;
    use std::panic;
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tonic::metadata::MetadataMap;

    const DEFAULT_TEST_SHORT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(100);

    // Sets up the test environment.
    //
    // Performs the following:
    // 1. Creates a work scheduler.
    // 2. Creates a fake channel that acts as a channel controller.
    // 3. Creates an StubPolicyBuilder with StubFuncs and the name of the test
    //    passed in.
    // 4. Create a Round Robin policy with the StubPolicyBuilder.
    //
    // Returns the following:
    // 1. A receiver for events initiated by the LB policy (like creating a new
    //    subchannel, sending a new picker etc).
    // 2. The Round Robin to send resolver and subchannel updates from the test.
    // 3. The controller to pass to the LB policy as part of the updates.
    fn setup(
        test_name: &'static str,
    ) -> (
        mpsc::UnboundedReceiver<TestEvent>,
        impl LbPolicy,
        Box<dyn ChannelController>,
    ) {
        pick_first::reg();
        round_robin::reg();
        test_utils::reg_stub_policy(test_name, create_funcs_for_roundrobin_tests());

        let (tx_events, rx_events) = mpsc::unbounded_channel();
        let work_scheduler = Arc::new(TestWorkScheduler {
            tx_events: tx_events.clone(),
        });
        let child_manager = ChildManager::new(default_runtime(), work_scheduler);
        let tcc = Box::new(TestChannelController { tx_events });
        let child_policy_builder = GLOBAL_LB_REGISTRY.get_policy(test_name).unwrap();
        let lb_policy = RoundRobinPolicy::new(child_manager, child_policy_builder);
        (rx_events, lb_policy, tcc)
    }

    struct TestSubchannelList {
        subchannels: Vec<Arc<dyn Subchannel>>,
    }

    impl TestSubchannelList {
        fn new(addresses: &Vec<Address>, channel_controller: &mut dyn ChannelController) -> Self {
            TestSubchannelList {
                subchannels: addresses
                    .into_iter()
                    .map(|a| channel_controller.new_subchannel(a))
                    .collect(),
            }
        }

        fn contains(&self, sc: &Arc<dyn Subchannel>) -> bool {
            self.subchannels.contains(sc)
        }
    }

    fn create_endpoints(num_endpoints: usize, num_addresses: usize) -> Vec<Endpoint> {
        let mut endpoints = Vec::with_capacity(num_endpoints);
        for i in 0..num_endpoints {
            let mut addresses: Vec<Address> = Vec::with_capacity(num_addresses);
            for j in 0..num_addresses {
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
        lb_policy: &mut impl LbPolicy,
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
        lb_policy: &mut impl LbPolicy,
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
        lb_policy: &mut impl LbPolicy,
        subchannel: Arc<dyn Subchannel>,
        state: ConnectivityState,
        tcc: &mut dyn ChannelController,
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
        lb_policy: &mut impl LbPolicy,
        subchannel: Arc<dyn Subchannel>,
        err: &str,
        tcc: &mut dyn ChannelController,
    ) {
        lb_policy.subchannel_update(
            subchannel,
            &SubchannelState {
                connectivity_state: ConnectivityState::TransientFailure,
                last_connection_error: Some(Arc::from(Box::from(err.to_owned()))),
            },
            tcc,
        );
    }

    #[derive(Debug)]
    struct OneSubchannelPicker {
        sc: Arc<dyn Subchannel>,
    }

    impl Picker for OneSubchannelPicker {
        fn pick(&self, request: &Request) -> PickResult {
            PickResult::Pick(Pick {
                subchannel: self.sc.clone(),
                on_complete: None,
                metadata: MetadataMap::new(),
            })
        }
    }

    fn addresses_from_endpoints(endpoints: &[Endpoint]) -> Vec<Address> {
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
    // robin. This is a simplified version of PickFirst. It just creates a
    // subchannel and then sends the appropriate picker update.
    fn create_funcs_for_roundrobin_tests() -> StubPolicyFuncs {
        StubPolicyFuncs {
            // Closure for resolver_update. It creates a subchannel for the
            // endpoint it receives and stores which endpoint it received and
            // which subchannel this child created in the data field.
            resolver_update: Some(Arc::new(
                |data: &mut StubPolicyData, update: ResolverUpdate, _, channel_controller| {
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
                    if let Err(error) = update.endpoints {
                        if state.addresses.is_empty()
                            || state.connectivity_state == ConnectivityState::TransientFailure
                        {
                            channel_controller.update_picker(LbState {
                                connectivity_state: ConnectivityState::TransientFailure,
                                picker: Arc::new(FailingPicker {
                                    error: error.to_string(),
                                }),
                            });
                            state.connectivity_state = ConnectivityState::TransientFailure;
                            channel_controller.request_resolution();
                        }
                        return Ok(());
                    };
                    let endpoints = update.endpoints.unwrap();
                    let new_addresses = addresses_from_endpoints(&endpoints);
                    if new_addresses.is_empty() {
                        channel_controller.update_picker(LbState {
                            connectivity_state: ConnectivityState::TransientFailure,
                            picker: Arc::new(FailingPicker {
                                error: "Received empty address list from the name resolver"
                                    .to_string(),
                            }),
                        });
                        state.connectivity_state = ConnectivityState::TransientFailure;
                        channel_controller.request_resolution();
                        return Err("Received empty address list from the name resolver".into());
                    }

                    if state.connectivity_state != ConnectivityState::Idle {
                        state.subchannel_list =
                            Some(TestSubchannelList::new(&new_addresses, channel_controller));
                    }
                    state.addresses = new_addresses;
                    Ok(())
                },
            )),
            // Closure for subchannel_update. Verify that the subchannel being
            // updated is the same one that this child policy created in
            // resolver_update. It then sends a picker of the same state that
            // was passed to it.
            subchannel_update: Some(Arc::new(
                |data: &mut StubPolicyData, subchannel, state, channel_controller| {
                    // Retrieve the specific TestState from the generic test_data field.
                    // This downcasts the `Any` trait object
                    let test_data = data.test_data.as_mut().unwrap(); // ? ignore?
                    let test_state = test_data.downcast_mut::<PickFirstState>().unwrap();
                    let scl = &mut test_state.subchannel_list.as_ref().unwrap();
                    assert!(
                        scl.contains(&subchannel),
                        "subchannel_update received an update for a subchannel it does not own."
                    );
                    test_state.connectivity_state = state.connectivity_state;
                    match state.connectivity_state {
                        ConnectivityState::Ready => {
                            channel_controller.update_picker(LbState {
                                connectivity_state: state.connectivity_state,
                                picker: Arc::new(OneSubchannelPicker { sc: subchannel }),
                            });
                        }
                        ConnectivityState::Idle => {}
                        ConnectivityState::Connecting => {
                            channel_controller.update_picker(LbState {
                                connectivity_state: state.connectivity_state,
                                picker: Arc::new(QueuingPicker {}),
                            });
                        }
                        ConnectivityState::TransientFailure => {
                            channel_controller.update_picker(LbState {
                                connectivity_state: state.connectivity_state,
                                picker: Arc::new(FailingPicker {
                                    error: state
                                        .last_connection_error
                                        .as_ref()
                                        .unwrap()
                                        .to_string(),
                                }),
                            });
                        }
                    }
                },
            )),
            work: None,
        }
    }

    // Creates a new endpoint with the specified number of addresses.
    fn create_endpoint(num_addresses: usize) -> Endpoint {
        let mut addresses = Vec::with_capacity(num_addresses);
        for i in 0..num_addresses {
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
    async fn verify_subchannel_creation(
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
    async fn verify_connecting_picker(
        rx_events: &mut mpsc::UnboundedReceiver<TestEvent>,
    ) -> Arc<dyn Picker> {
        println!("verify connecting picker");
        match rx_events.recv().await.unwrap() {
            TestEvent::UpdatePicker(update) => {
                println!("connectivity state is {}", update.connectivity_state);
                assert!(update.connectivity_state == ConnectivityState::Connecting);
                let req = test_utils::new_request();
                assert!(update.picker.pick(&req) == PickResult::Queue);
                update.picker
            }
            other => panic!("unexpected event {:?}", other),
        }
    }

    // Verifies that the channel moves to READY state with a picker that returns
    // the given subchannel.
    //
    // Returns the picker for tests to make more picks, if required.
    async fn verify_ready_picker(
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
    async fn verify_roundrobin_ready_picker(
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
    async fn verify_transient_failure_picker(
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
                        dbg!(status.message());
                        dbg!(&want_error);
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

    async fn verify_no_activity(rx_events: &mut mpsc::UnboundedReceiver<TestEvent>) {
        tokio::select! {
            _ = tokio::time::sleep(DEFAULT_TEST_SHORT_TIMEOUT) => {}
            event = rx_events.recv() => {
                panic!("unexpected event {:?}", event.unwrap());
            }
        }
    }

    // Tests the scenario where the resolver returns an error before a valid
    // update. The LB policy should move to TRANSIENT_FAILURE state with a
    // failing picker.
    #[tokio::test]
    async fn roundrobin_resolver_error_before_a_valid_update() {
        let (mut rx_events, mut lb_policy, mut tcc) =
            setup("stub-roundrobin_resolver_error_before_a_valid_update");
        let tcc = tcc.as_mut();
        let resolver_error = String::from("resolver error");
        send_resolver_error_to_policy(&mut lb_policy, resolver_error.clone(), tcc);
        verify_transient_failure_picker(&mut rx_events, resolver_error).await;
    }

    // Tests the scenario where the resolver returns an error after a valid update
    // and the LB policy has moved to READY. The LB policy should ignore the error
    // and continue using the previously received update.
    #[tokio::test]
    async fn roundrobin_resolver_error_after_a_valid_update_in_ready() {
        let (mut rx_events, mut lb_policy, mut tcc) =
            setup("stub-roundrobin_resolver_error_after_a_valid_update_in_ready");
        let tcc = tcc.as_mut();
        let endpoint = create_endpoint(1);
        send_resolver_update_to_policy(&mut lb_policy, vec![endpoint], tcc);
        let subchannels = verify_subchannel_creation(&mut rx_events, 1).await;

        move_subchannel_to_state(
            &mut lb_policy,
            subchannels[0].clone(),
            ConnectivityState::Connecting,
            tcc,
        );
        verify_connecting_picker(&mut rx_events).await;

        move_subchannel_to_state(
            &mut lb_policy,
            subchannels[0].clone(),
            ConnectivityState::Ready,
            tcc,
        );
        let picker = verify_ready_picker(&mut rx_events, subchannels[0].clone()).await;
        let resolver_error = String::from("resolver error");
        send_resolver_error_to_policy(&mut lb_policy, resolver_error.clone(), tcc);
        verify_no_activity(&mut rx_events).await;

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
        let (mut rx_events, mut lb_policy, mut tcc) =
            setup("stub-roundrobin_resolver_error_after_a_valid_update_in_connecting");
        let tcc = tcc.as_mut();

        let endpoint = create_endpoint(1);
        send_resolver_update_to_policy(&mut lb_policy, vec![endpoint], tcc);
        let subchannels = verify_subchannel_creation(&mut rx_events, 1).await;

        move_subchannel_to_state(
            &mut lb_policy,
            subchannels[0].clone(),
            ConnectivityState::Connecting,
            tcc,
        );
        let picker = verify_connecting_picker(&mut rx_events).await;

        let resolver_error = String::from("resolver error");

        send_resolver_error_to_policy(&mut lb_policy, resolver_error, tcc);

        verify_no_activity(&mut rx_events).await;

        let req = test_utils::new_request();
        match picker.pick(&req) {
            PickResult::Queue => {}
            other => panic!("unexpected pick result {}", other),
        }
    }

    // Tests the scenario where the resolver returns an error after a valid
    // update and the LB policy has moved to TRANSIENT_FAILURE after attempting
    // to connect to all addresses. The LB policy should send a new picker that
    // returns the error from the resolver.
    #[tokio::test]
    async fn roundrobin_resolver_error_after_a_valid_update_in_tf() {
        let (mut rx_events, mut lb_policy, mut tcc) =
            setup("stub-roundrobin_resolver_error_after_a_valid_update_in_tf");
        let tcc = tcc.as_mut();
        let endpoint = create_endpoint(1);
        send_resolver_update_to_policy(&mut lb_policy, vec![endpoint], tcc);
        let subchannels = verify_subchannel_creation(&mut rx_events, 1).await;
        move_subchannel_to_state(
            &mut lb_policy,
            subchannels[0].clone(),
            ConnectivityState::Connecting,
            tcc,
        );
        verify_connecting_picker(&mut rx_events).await;
        let connection_error = String::from("test connection error");
        move_subchannel_to_transient_failure(
            &mut lb_policy,
            subchannels[0].clone(),
            &connection_error,
            tcc,
        );
        verify_transient_failure_picker(&mut rx_events, connection_error).await;
        let resolver_error = String::from("resolver error");
        send_resolver_error_to_policy(&mut lb_policy, resolver_error.clone(), tcc);
        verify_resolution_request(&mut rx_events).await;
        verify_transient_failure_picker(&mut rx_events, resolver_error).await;
    }

    // Round Robin should round robin across endpoints.
    #[tokio::test]
    async fn roundrobin_picks_are_round_robin() {
        let (mut rx_events, mut lb_policy, mut tcc) =
            setup("stub-roundrobin_picks_are_round_robin");
        let tcc = tcc.as_mut();
        let endpoints = create_endpoints(2, 1);
        send_resolver_update_to_policy(&mut lb_policy, endpoints, tcc);
        let subchannels = verify_subchannel_creation(&mut rx_events, 2).await;
        move_subchannel_to_state(
            &mut lb_policy,
            subchannels[0].clone(),
            ConnectivityState::Connecting,
            tcc,
        );
        verify_connecting_picker(&mut rx_events).await;
        move_subchannel_to_state(
            &mut lb_policy,
            subchannels[0].clone(),
            ConnectivityState::Ready,
            tcc,
        );
        verify_ready_picker(&mut rx_events, subchannels[0].clone()).await;
        move_subchannel_to_state(
            &mut lb_policy,
            subchannels[1].clone(),
            ConnectivityState::Ready,
            tcc,
        );
        let picker = verify_roundrobin_ready_picker(&mut rx_events).await;
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
        assert!(picked.contains(&subchannels[1]));
    }

    // If round robin receives no endpoints in a resolver update,
    // it should go into transient failure.
    #[tokio::test]
    async fn roundrobin_endpoints_removed() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup("stub-roundrobin_addresses_removed");
        let tcc = tcc.as_mut();

        let endpoints = create_endpoints(2, 1);
        send_resolver_update_to_policy(&mut lb_policy, endpoints, tcc);
        let subchannels = verify_subchannel_creation(&mut rx_events, 2).await;
        move_subchannel_to_state(
            &mut lb_policy,
            subchannels[0].clone(),
            ConnectivityState::Connecting,
            tcc,
        );
        verify_connecting_picker(&mut rx_events).await;
        move_subchannel_to_state(
            &mut lb_policy,
            subchannels[0].clone(),
            ConnectivityState::Connecting,
            tcc,
        );
        verify_connecting_picker(&mut rx_events).await;
        let update = ResolverUpdate {
            endpoints: Ok(vec![]),
            ..Default::default()
        };
        let _ = lb_policy.resolver_update(update, None, tcc);
        let want_error = "Received empty address list from the name resolver";
        verify_transient_failure_picker(&mut rx_events, want_error.to_string()).await;
        verify_resolution_request(&mut rx_events).await;
    }

    // Round robin should only round robin across children that are ready.
    // If a child leaves the ready state, Round Robin should only
    // pick from the children that are still Ready.
    #[tokio::test]
    async fn roundrobin_one_endpoint_down() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup("stub-roundrobin_one_endpoint_down");
        let tcc = tcc.as_mut();
        let endpoints = create_endpoints(2, 1);
        send_resolver_update_to_policy(&mut lb_policy, endpoints, tcc);
        let subchannels = verify_subchannel_creation(&mut rx_events, 2).await;
        move_subchannel_to_state(
            &mut lb_policy,
            subchannels[0].clone(),
            ConnectivityState::Connecting,
            tcc,
        );
        verify_connecting_picker(&mut rx_events).await;
        move_subchannel_to_state(
            &mut lb_policy,
            subchannels[0].clone(),
            ConnectivityState::Ready,
            tcc,
        );
        let picker = verify_ready_picker(&mut rx_events, subchannels[0].clone()).await;
        move_subchannel_to_state(
            &mut lb_policy,
            subchannels[1].clone(),
            ConnectivityState::Ready,
            tcc,
        );
        let picker = verify_roundrobin_ready_picker(&mut rx_events).await;
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
        assert!(picked.contains(&subchannels[1]));
        let subchannel_being_removed = subchannels[1].clone();
        let error = "endpoint down";
        move_subchannel_to_transient_failure(&mut lb_policy, subchannels[1].clone(), error, tcc);

        let new_picker = verify_roundrobin_ready_picker(&mut rx_events).await;

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
    // should not be a part of its picks anymore and should be removed. It should
    // then roundrobin across the endpoints it still has and the new one.
    #[tokio::test]
    async fn roundrobin_pick_after_resolved_updated_hosts() {
        let (mut rx_events, mut lb_policy, mut tcc) =
            setup("stub-roundrobin_pick_after_resolved_updated_hosts");
        let tcc = tcc.as_mut();

        // Two initial endpoints: subchannel_one, subchannel_two
        let addr_one = Address {
            address: "subchannel_one".to_string().into(),
            ..Default::default()
        };
        let addr_two = Address {
            address: "subchannel_two".to_string().into(),
            ..Default::default()
        };
        let endpoint_one = Endpoint {
            addresses: vec![addr_one],
            ..Default::default()
        };
        let endpoint_two = Endpoint {
            addresses: vec![addr_two],
            ..Default::default()
        };

        send_resolver_update_to_policy(
            &mut lb_policy,
            vec![endpoint_one, endpoint_two.clone()],
            tcc,
        );

        // Start with two subchannels created
        let all_subchannels = verify_subchannel_creation(&mut rx_events, 2).await;
        let subchannel_one = all_subchannels
            .iter()
            .find(|sc| sc.address().address == "subchannel_one".to_string().into())
            .unwrap();
        let subchannel_two = all_subchannels
            .iter()
            .find(|sc| sc.address().address == "subchannel_two".to_string().into())
            .unwrap();

        move_subchannel_to_state(
            &mut lb_policy,
            subchannel_one.clone(),
            ConnectivityState::Connecting,
            tcc,
        );
        verify_connecting_picker(&mut rx_events).await;
        move_subchannel_to_state(
            &mut lb_policy,
            subchannel_two.clone(),
            ConnectivityState::Connecting,
            tcc,
        );
        verify_connecting_picker(&mut rx_events).await;

        move_subchannel_to_state(
            &mut lb_policy,
            subchannel_one.clone(),
            ConnectivityState::Ready,
            tcc,
        );
        verify_ready_picker(&mut rx_events, subchannel_one.clone()).await;
        move_subchannel_to_state(
            &mut lb_policy,
            subchannel_two.clone(),
            ConnectivityState::Ready,
            tcc,
        );
        let picker = verify_roundrobin_ready_picker(&mut rx_events).await;

        let req = test_utils::new_request();
        let mut picked = Vec::new();
        for _ in 0..4 {
            match picker.pick(&req) {
                PickResult::Pick(pick) => picked.push(pick.subchannel.clone()),
                other => panic!("unexpected pick result {}", other),
            }
        }
        assert!(picked.contains(subchannel_one));
        assert!(picked.contains(subchannel_two));

        // Resolver update removes subchannel_one and adds "new"
        let new_addr = Address {
            address: "new".to_string().into(),
            ..Default::default()
        };
        let new_endpoint = Endpoint {
            addresses: vec![new_addr],
            ..Default::default()
        };

        send_resolver_update_to_policy(&mut lb_policy, vec![endpoint_two, new_endpoint], tcc);

        let new_subchannels = verify_subchannel_creation(&mut rx_events, 2).await;
        let new_sc = new_subchannels
            .iter()
            .find(|sc| sc.address().address == "new".to_string().into())
            .unwrap();
        let old_sc = new_subchannels
            .iter()
            .find(|sc| sc.address().address == "subchannel_two".to_string().into())
            .unwrap();

        move_subchannel_to_state(
            &mut lb_policy,
            old_sc.clone(),
            ConnectivityState::Ready,
            tcc,
        );
        let _ = verify_roundrobin_ready_picker(&mut rx_events).await;

        move_subchannel_to_state(
            &mut lb_policy,
            new_sc.clone(),
            ConnectivityState::Connecting,
            tcc,
        );
        let _ = verify_roundrobin_ready_picker(&mut rx_events).await;
        move_subchannel_to_state(
            &mut lb_policy,
            new_sc.clone(),
            ConnectivityState::Ready,
            tcc,
        );
        let new_picker = verify_roundrobin_ready_picker(&mut rx_events).await;

        let req = test_utils::new_request();
        let mut picked = Vec::new();
        for _ in 0..4 {
            match new_picker.pick(&req) {
                PickResult::Pick(pick) => picked.push(pick.subchannel.clone()),
                other => panic!("unexpected pick result {}", other),
            }
        }
        assert!(picked.contains(old_sc));
        assert!(picked.contains(new_sc));
        assert!(!picked.contains(subchannel_one));
    }

    // Round robin should stay in transient failure until a child reports ready
    #[tokio::test]
    async fn roundrobin_stay_transient_failure_until_ready() {
        let (mut rx_events, mut lb_policy, mut tcc) =
            setup("stub-roundrobin_stay_transient_failure_until_ready");
        let tcc = tcc.as_mut();
        let endpoints = create_endpoints(2, 1);
        send_resolver_update_to_policy(&mut lb_policy, endpoints, tcc);
        let subchannels = verify_subchannel_creation(&mut rx_events, 2).await;
        move_subchannel_to_state(
            &mut lb_policy,
            subchannels[0].clone(),
            ConnectivityState::Connecting,
            tcc,
        );
        verify_connecting_picker(&mut rx_events).await;
        move_subchannel_to_state(
            &mut lb_policy,
            subchannels[1].clone(),
            ConnectivityState::Connecting,
            tcc,
        );
        verify_connecting_picker(&mut rx_events).await;
        let first_error = String::from("test connection error 1");
        move_subchannel_to_transient_failure(
            &mut lb_policy,
            subchannels[0].clone(),
            &first_error,
            tcc,
        );
        verify_connecting_picker(&mut rx_events).await;
        move_subchannel_to_transient_failure(
            &mut lb_policy,
            subchannels[1].clone(),
            &first_error,
            tcc,
        );
        verify_transient_failure_picker(&mut rx_events, first_error).await;
        move_subchannel_to_state(
            &mut lb_policy,
            subchannels[0].clone(),
            ConnectivityState::Ready,
            tcc,
        );
        verify_ready_picker(&mut rx_events, subchannels[0].clone()).await;
    }

    // Tests the scenario where the resolver returns an update with no endpoints
    // (before sending any valid update). The LB policy should move to
    // TRANSIENT_FAILURE state with a failing picker.
    #[tokio::test]
    async fn roundrobin_zero_endpoints_from_resolver_before_valid_update() {
        let (mut rx_events, mut lb_policy, mut tcc) =
            setup("stub-roundrobin_zero_endpoints_from_resolver_before_valid_update");
        let tcc = tcc.as_mut();
        send_resolver_update_to_policy(&mut lb_policy, vec![], tcc);
        verify_transient_failure_picker(
            &mut rx_events,
            "Received empty address list from the name resolver".to_string(),
        )
        .await;
    }

    // Tests the scenario where the resolver returns an update with no endpoints
    // after sending a valid update (and the LB policy has moved to READY). The LB
    // policy should move to TRANSIENT_FAILURE state with a failing picker.
    #[tokio::test]
    async fn roundrobin_zero_endpoints_from_resolver_after_valid_update() {
        let (mut rx_events, mut lb_policy, mut tcc) =
            setup("stub-roundrobin_zero_endpoints_from_resolver_after_valid_update");
        let tcc = tcc.as_mut();

        let endpoint = create_endpoint(1);
        send_resolver_update_to_policy(&mut lb_policy, vec![endpoint], tcc);
        let subchannels = verify_subchannel_creation(&mut rx_events, 1).await;
        move_subchannel_to_state(
            &mut lb_policy,
            subchannels[0].clone(),
            ConnectivityState::Connecting,
            tcc,
        );
        verify_connecting_picker(&mut rx_events).await;
        move_subchannel_to_state(
            &mut lb_policy,
            subchannels[0].clone(),
            ConnectivityState::Ready,
            tcc,
        );
        verify_ready_picker(&mut rx_events, subchannels[0].clone()).await;
        let update = ResolverUpdate {
            endpoints: Ok(vec![]),
            ..Default::default()
        };
        assert!(lb_policy.resolver_update(update, None, tcc).is_err());
        verify_transient_failure_picker(
            &mut rx_events,
            "Received empty address list from the name resolver".to_string(),
        )
        .await;
        verify_resolution_request(&mut rx_events).await;
    }

    // Tests the scenario where the resolver returns an update with multiple
    // address. The LB policy should create subchannels for all address, and attempt
    // to connect to them in order, until a connection succeeds, at which point it
    // should move to READY state with a picker that returns that subchannel.
    #[tokio::test]
    async fn roundrobin_with_multiple_backends_first_backend_is_ready() {
        let (mut rx_events, mut lb_policy, mut tcc) =
            setup("stub-roundrobin_with_multiple_backends_first_backend_is_ready");
        let tcc = tcc.as_mut();

        let endpoint = create_endpoints(2, 1);
        send_resolver_update_to_policy(&mut lb_policy, endpoint, tcc);
        let subchannels = verify_subchannel_creation(&mut rx_events, 2).await;

        move_subchannel_to_state(
            &mut lb_policy,
            subchannels[0].clone(),
            ConnectivityState::Connecting,
            tcc,
        );
        verify_connecting_picker(&mut rx_events).await;
        move_subchannel_to_state(
            &mut lb_policy,
            subchannels[0].clone(),
            ConnectivityState::Ready,
            tcc,
        );

        let picker = verify_ready_picker(&mut rx_events, subchannels[0].clone()).await;

        let req = test_utils::new_request();
        // First pick determines the only subchannel the picker should yield
        let first_sc = match picker.pick(&req) {
            PickResult::Pick(p) => p.subchannel.clone(),
            other => panic!("unexpected pick result {}", other),
        };

        for _ in 0..7 {
            match picker.pick(&req) {
                PickResult::Pick(p) => {
                    assert!(
                        Arc::ptr_eq(&first_sc, &p.subchannel),
                        "READY picker should contain exactly one subchannel"
                    );
                }
                other => panic!("unexpected pick result {}", other),
            }
        }
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
        let (mut rx_events, mut lb_policy, mut tcc) =
            setup("stub-roundrobin_resolver_update_contains_currently_ready_subchannel");
        let tcc = tcc.as_mut();

        let endpoints = create_endpoint(2);
        send_resolver_update_to_policy(&mut lb_policy, vec![endpoints], tcc);
        let subchannels = verify_subchannel_creation(&mut rx_events, 2).await;
        move_subchannel_to_state(
            &mut lb_policy,
            subchannels[0].clone(),
            ConnectivityState::Connecting,
            tcc,
        );
        verify_connecting_picker(&mut rx_events).await;
        move_subchannel_to_state(
            &mut lb_policy,
            subchannels[0].clone(),
            ConnectivityState::Ready,
            tcc,
        );
        verify_ready_picker(&mut rx_events, subchannels[0].clone()).await;

        let mut endpoints = create_endpoint(4);
        endpoints.addresses.reverse();
        send_resolver_update_to_policy(&mut lb_policy, vec![endpoints], tcc);
        let subchannels = verify_subchannel_creation(&mut rx_events, 4).await;
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
        verify_ready_picker(&mut rx_events, subchannels[3].clone()).await;
    }
}
