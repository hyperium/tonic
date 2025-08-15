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

//! A utility which helps parent LB policies manage multiple children for the
//! purposes of forwarding channel updates.

// TODO: This is mainly provided as a fairly complex example of the current LB
// policy in use.  Complete tests must be written before it can be used in
// production.  Also, support for the work scheduler is missing.

use std::collections::HashSet;
use std::sync::Mutex;
use std::{collections::HashMap, error::Error, hash::Hash, mem, sync::Arc};

use crate::client::load_balancing::{
    ChannelController, LbConfig, LbPolicy, LbPolicyBuilder, LbPolicyOptions, LbState,
    WeakSubchannel, WorkScheduler,
};
use crate::client::name_resolution::{Address, ResolverUpdate};
use crate::client::ConnectivityState;
use crate::rt::Runtime;

use super::{Subchannel, SubchannelState};

// An LbPolicy implementation that manages multiple children.
pub struct ChildManager<T> {
    subchannel_child_map: HashMap<WeakSubchannel, usize>,
    children: Vec<Child<T>>,
    update_sharder: Box<dyn ResolverUpdateSharder<T>>,
    pending_work: Arc<Mutex<HashSet<usize>>>,
    runtime: Arc<dyn Runtime>,
}

struct Child<T> {
    identifier: T,
    policy: Box<dyn LbPolicy>,
    state: LbState,
    work_scheduler: Arc<ChildWorkScheduler>,
}

/// A collection of data sent to a child of the ChildManager.
pub struct ChildUpdate<T> {
    /// The identifier the ChildManager should use for this child.
    pub child_identifier: T,
    /// The builder the ChildManager should use to create this child if it does
    /// not exist.
    pub child_policy_builder: Arc<dyn LbPolicyBuilder>,
    /// The relevant ResolverUpdate to send to this child.
    pub child_update: ResolverUpdate,
}

pub trait ResolverUpdateSharder<T>: Send {
    /// Performs the operation of sharding an aggregate ResolverUpdate into one
    /// or more ChildUpdates.  Called automatically by the ChildManager when its
    /// resolver_update method is called.  The key in the returned map is the
    /// identifier the ChildManager should use for this child.
    fn shard_update(
        &self,
        resolver_update: ResolverUpdate,
    ) -> Result<Box<dyn Iterator<Item = ChildUpdate<T>>>, Box<dyn Error + Send + Sync>>;
}

impl<T> ChildManager<T> {
    /// Creates a new ChildManager LB policy.  shard_update is called whenever a
    /// resolver_update operation occurs.
    pub fn new(
        update_sharder: Box<dyn ResolverUpdateSharder<T>>,
        runtime: Arc<dyn Runtime>,
    ) -> Self {
        Self {
            update_sharder,
            subchannel_child_map: Default::default(),
            children: Default::default(),
            pending_work: Default::default(),
            runtime,
        }
    }

    /// Returns data for all current children.
    pub fn child_states(&mut self) -> impl Iterator<Item = (&T, &LbState)> {
        self.children
            .iter()
            .map(|child| (&child.identifier, &child.state))
    }

    /// Aggregates states from child policies.
    /// If any child is READY then we consider the aggregate state to be READY.
    /// Otherwise, if any child is CONNECTING, then report CONNECTING.
    /// Otherwise, if any child is IDLE, then report IDLE.
    /// Report TRANSIENT FAILURE if no conditions above apply.
    pub fn aggregate_states(&mut self) -> ConnectivityState {
        let mut is_connecting = false;
        let mut is_idle = false;

        for child in &self.children {
            match child.state.connectivity_state {
                ConnectivityState::Ready => {
                    return ConnectivityState::Ready;
                }
                ConnectivityState::Connecting => {
                    is_connecting = true;
                }
                ConnectivityState::Idle => {
                    is_idle = true;
                }
                ConnectivityState::TransientFailure => {}
            }
        }

        // Decide the new aggregate state if no child is READY.
        if is_connecting {
            ConnectivityState::Connecting
        } else if is_idle {
            ConnectivityState::Idle
        } else {
            ConnectivityState::TransientFailure
        }
    }

    // Called to update all accounting in the ChildManager from operations
    // performed by a child policy on the WrappedController that was created for
    // it.  child_idx is an index into the children map for the relevant child.
    //
    // TODO: this post-processing step can be eliminated by capturing the right
    // state inside the WrappedController, however it is fairly complex.  Decide
    // which way is better.
    fn resolve_child_controller(
        &mut self,
        channel_controller: WrappedController,
        child_idx: usize,
    ) {
        // Add all created subchannels into the subchannel_child_map.
        for csc in channel_controller.created_subchannels {
            self.subchannel_child_map.insert(csc.into(), child_idx);
        }
        // Update the tracked state if the child produced an update.
        if let Some(state) = channel_controller.picker_update {
            self.children[child_idx].state = state;
        };
    }
}

impl<T: PartialEq + Hash + Eq + Send + Sync + 'static> LbPolicy for ChildManager<T> {
    fn resolver_update(
        &mut self,
        resolver_update: ResolverUpdate,
        config: Option<&LbConfig>,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // First determine if the incoming update is valid.
        let child_updates = self.update_sharder.shard_update(resolver_update)?;

        // Hold the lock to prevent new work requests during this operation and
        // rewrite the indices.
        let mut pending_work = self.pending_work.lock().unwrap();

        // Reset pending work; we will re-add any entries it contains with the
        // right index later.
        let old_pending_work = mem::take(&mut *pending_work);

        // Replace self.children with an empty vec.
        let old_children = mem::take(&mut self.children);

        // Replace the subchannel map with an empty map.
        let old_subchannel_child_map = mem::take(&mut self.subchannel_child_map);

        // Reverse the old subchannel map.
        let mut old_child_subchannels_map: HashMap<usize, Vec<WeakSubchannel>> = HashMap::new();

        for (subchannel, child_idx) in old_subchannel_child_map {
            old_child_subchannels_map
                .entry(child_idx)
                .or_default()
                .push(subchannel);
        }

        // Build a map of the old children from their IDs for efficient lookups.
        let old_children = old_children
            .into_iter()
            .enumerate()
            .map(|(old_idx, e)| (e.identifier, (e.policy, e.state, old_idx, e.work_scheduler)));
        let mut old_children: HashMap<T, _> = old_children.collect();

        // Split the child updates into the IDs and builders, and the
        // ResolverUpdates.
        let (ids_builders, updates): (Vec<_>, Vec<_>) = child_updates
            .map(|e| ((e.child_identifier, e.child_policy_builder), e.child_update))
            .unzip();

        // Transfer children whose identifiers appear before and after the
        // update, and create new children.  Add entries back into the
        // subchannel map.
        for (new_idx, (identifier, builder)) in ids_builders.into_iter().enumerate() {
            if let Some((policy, state, old_idx, work_scheduler)) = old_children.remove(&identifier)
            {
                for subchannel in old_child_subchannels_map
                    .remove(&old_idx)
                    .into_iter()
                    .flatten()
                {
                    self.subchannel_child_map.insert(subchannel, new_idx);
                }
                if old_pending_work.contains(&old_idx) {
                    pending_work.insert(new_idx);
                }
                *work_scheduler.idx.lock().unwrap() = Some(new_idx);
                self.children.push(Child {
                    identifier,
                    state,
                    policy,
                    work_scheduler,
                });
            } else {
                let work_scheduler = Arc::new(ChildWorkScheduler {
                    pending_work: self.pending_work.clone(),
                    idx: Mutex::new(Some(new_idx)),
                });
                let policy = builder.build(LbPolicyOptions {
                    work_scheduler: work_scheduler.clone(),
                    runtime: self.runtime.clone(),
                });
                let state = LbState::initial();
                self.children.push(Child {
                    identifier,
                    state,
                    policy,
                    work_scheduler,
                });
            };
        }

        // Invalidate all deleted children's work_schedulers.
        for (_, (_, _, _, work_scheduler)) in old_children {
            *work_scheduler.idx.lock().unwrap() = None;
        }

        // Release the pending_work mutex before calling into the children to
        // allow their work scheduler calls to unblock.
        drop(pending_work);

        // Anything left in old_children will just be Dropped and cleaned up.

        // Call resolver_update on all children.
        let mut updates = updates.into_iter();
        for child_idx in 0..self.children.len() {
            let child = &mut self.children[child_idx];
            let child_update = updates.next().unwrap();
            let mut channel_controller = WrappedController::new(channel_controller);
            let _ = child
                .policy
                .resolver_update(child_update, config, &mut channel_controller);
            self.resolve_child_controller(channel_controller, child_idx);
        }
        Ok(())
    }

    fn subchannel_update(
        &mut self,
        subchannel: Arc<dyn Subchannel>,
        state: &SubchannelState,
        channel_controller: &mut dyn ChannelController,
    ) {
        // Determine which child created this subchannel.
        let child_idx = *self
            .subchannel_child_map
            .get(&WeakSubchannel::new(&subchannel))
            .unwrap();
        let policy = &mut self.children[child_idx].policy;
        // Wrap the channel_controller to track the child's operations.
        let mut channel_controller = WrappedController::new(channel_controller);
        // Call the proper child.
        policy.subchannel_update(subchannel, state, &mut channel_controller);
        self.resolve_child_controller(channel_controller, child_idx);
    }

    fn work(&mut self, channel_controller: &mut dyn ChannelController) {
        let child_idxes = mem::take(&mut *self.pending_work.lock().unwrap());
        for child_idx in child_idxes {
            let mut channel_controller = WrappedController::new(channel_controller);
            self.children[child_idx]
                .policy
                .work(&mut channel_controller);
            self.resolve_child_controller(channel_controller, child_idx);
        }
    }

    fn exit_idle(&mut self, _channel_controller: &mut dyn ChannelController) {
        todo!("implement exit_idle")
    }
}

struct WrappedController<'a> {
    channel_controller: &'a mut dyn ChannelController,
    created_subchannels: Vec<Arc<dyn Subchannel>>,
    picker_update: Option<LbState>,
}

impl<'a> WrappedController<'a> {
    fn new(channel_controller: &'a mut dyn ChannelController) -> Self {
        Self {
            channel_controller,
            created_subchannels: vec![],
            picker_update: None,
        }
    }
}

impl ChannelController for WrappedController<'_> {
    fn new_subchannel(&mut self, address: &Address) -> Arc<dyn Subchannel> {
        let subchannel = self.channel_controller.new_subchannel(address);
        self.created_subchannels.push(subchannel.clone());
        subchannel
    }

    fn update_picker(&mut self, update: LbState) {
        self.picker_update = Some(update);
    }

    fn request_resolution(&mut self) {
        self.channel_controller.request_resolution();
    }
}

struct ChildWorkScheduler {
    pending_work: Arc<Mutex<HashSet<usize>>>, // Must be taken first for correctness
    idx: Mutex<Option<usize>>,                // None if the child is deleted.
}

impl WorkScheduler for ChildWorkScheduler {
    fn schedule_work(&self) {
        let mut pending_work = self.pending_work.lock().unwrap();
        if let Some(idx) = *self.idx.lock().unwrap() {
            pending_work.insert(idx);
        }
    }
}

#[cfg(test)]
mod test {
    use crate::client::load_balancing::child_manager::{
        Child, ChildManager, ChildUpdate, ChildWorkScheduler, ResolverUpdateSharder,
    };
    use crate::client::load_balancing::test_utils::{
        self, Data, PolicyFuncs, StubPolicy, StubPolicyBuilder, TestChannelController, TestEvent,
        TestSubchannel, TestWorkScheduler,
    };
    use crate::client::load_balancing::{
        ChannelController, LbPolicy, LbPolicyBuilder, LbPolicyOptions, LbState, ParsedJsonLbConfig,
        Pick, PickResult, Picker, Subchannel, SubchannelState, GLOBAL_LB_REGISTRY,
    };
    use crate::client::name_resolution::{Address, Endpoint, ResolverUpdate};
    use crate::client::service_config::{LbConfig, ServiceConfig};
    use crate::client::ConnectivityState;
    use crate::rt::{default_runtime, Runtime};
    use crate::service::Request;
    use ::std::collections::{HashMap, HashSet};
    use ::std::error::Error;
    use ::std::panic;
    use ::std::sync::Arc;
    use serde::{Deserialize, Serialize};
    use std::sync::Mutex;
    use tokio::sync::mpsc;
    use tonic::metadata::MetadataMap;

    /// This picker is for testing purposes.
    pub struct DummyPicker {
        name: &'static str,
    }

    impl DummyPicker {
        pub fn new(name: &'static str) -> Self {
            Self { name }
        }
    }
    impl Picker for DummyPicker {
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

    /// This EndpointSharder maps endpoints to children policies.
    pub struct EndpointSharder {
        builder: Arc<dyn LbPolicyBuilder>,
    }

    impl ResolverUpdateSharder<Endpoint> for EndpointSharder {
        fn shard_update(
            &self,
            resolver_update: ResolverUpdate,
        ) -> Result<Box<dyn Iterator<Item = ChildUpdate<Endpoint>>>, Box<dyn Error + Send + Sync>>
        {
            let mut endpoint_to_child_map = HashMap::new();
            for endpoint in resolver_update.endpoints.clone().unwrap().iter() {
                let child_update = ChildUpdate {
                    child_identifier: endpoint.clone(),
                    child_policy_builder: self.builder.clone(),
                    child_update: ResolverUpdate {
                        attributes: resolver_update.attributes.clone(),
                        endpoints: Ok(vec![endpoint.clone()]),
                        service_config: resolver_update.service_config.clone(),
                        resolution_note: resolver_update.resolution_note.clone(),
                    },
                };
                endpoint_to_child_map.insert(endpoint.clone(), child_update);
            }
            Ok(Box::new(endpoint_to_child_map.into_values()))
        }
    }

    // Sends a resolver update to the LB policy with the specified endpoint.
    fn send_resolver_update_to_policy(
        lb_policy: &mut dyn LbPolicy,
        endpoints: Vec<Endpoint>,
        tcc: &mut dyn ChannelController,
    ) {
        let update = ResolverUpdate {
            endpoints: Ok(endpoints.clone()),
            ..Default::default()
        };
        assert!(lb_policy.resolver_update(update, None, tcc).is_ok());
    }

    fn setup(
        funcs: PolicyFuncs,
    ) -> (
        mpsc::UnboundedReceiver<TestEvent>,
        Box<ChildManager<Endpoint>>,
        Box<dyn ChannelController>,
    ) {
        let (tx_events, rx_events) = mpsc::unbounded_channel::<TestEvent>();
        let work_scheduler = Arc::new(TestWorkScheduler {
            tx_events: tx_events.clone(),
        });

        let tcc = Box::new(TestChannelController {
            tx_events: tx_events.clone(),
        });
        let builder = StubPolicyBuilder::new("reusable-stub-policy", funcs);

        let endpoint_sharder = EndpointSharder {
            builder: Arc::new(builder),
        };
        let child_manager = ChildManager::new(Box::new(endpoint_sharder), default_runtime());
        (rx_events, Box::new(child_manager), tcc)
    }

    fn create_n_endpoints_with_k_addresses(n: usize, k: usize) -> Vec<Endpoint> {
        let mut addresses = Vec::new();
        let mut endpoints = Vec::new();
        for i in 0..n {
            let mut n_addresses = Vec::new();
            for j in 0..k {
                n_addresses.push(Address {
                    address: format!("{}.{}.{}.{}:{}", j + i, j + i, j + i, j + i, j + i)
                        .to_string()
                        .into(),
                    ..Default::default()
                });
            }
            addresses.push(n_addresses);
            endpoints.push(Endpoint {
                addresses: addresses[i].clone(),
                ..Default::default()
            })
        }
        endpoints
    }

    fn move_subchannel_to_idle(
        lb_policy: &mut dyn LbPolicy,
        subchannel: Arc<dyn Subchannel>,
        tcc: &mut dyn ChannelController,
    ) {
        lb_policy.subchannel_update(
            subchannel.clone(),
            &SubchannelState {
                connectivity_state: ConnectivityState::Idle,
                ..Default::default()
            },
            tcc,
        );
    }

    fn move_subchannel_to_connecting(
        lb_policy: &mut dyn LbPolicy,
        subchannel: Arc<dyn Subchannel>,
        tcc: &mut dyn ChannelController,
    ) {
        lb_policy.subchannel_update(
            subchannel.clone(),
            &SubchannelState {
                connectivity_state: ConnectivityState::Connecting,
                ..Default::default()
            },
            tcc,
        );
    }

    fn move_subchannel_to_ready(
        lb_policy: &mut dyn LbPolicy,
        subchannel: Arc<dyn Subchannel>,
        tcc: &mut dyn ChannelController,
    ) {
        lb_policy.subchannel_update(
            subchannel.clone(),
            &SubchannelState {
                connectivity_state: ConnectivityState::Ready,
                ..Default::default()
            },
            tcc,
        );
    }

    fn move_subchannel_to_transient_failure(
        lb_policy: &mut dyn LbPolicy,
        subchannel: Arc<dyn Subchannel>,
        tcc: &mut dyn ChannelController,
    ) {
        lb_policy.subchannel_update(
            subchannel.clone(),
            &SubchannelState {
                connectivity_state: ConnectivityState::TransientFailure,
                ..Default::default()
            },
            tcc,
        );
    }

    // Verifies that the subchannels are created for the given addresses in the
    // given order. Returns the subchannels created.
    async fn verify_subchannel_creation_from_policy(
        rx_events: &mut mpsc::UnboundedReceiver<TestEvent>,
        addresses: Vec<Address>,
    ) -> Vec<Arc<dyn Subchannel>> {
        let mut subchannels = Vec::new();
        for address in addresses {
            match rx_events.recv().await.unwrap() {
                TestEvent::NewSubchannel(sc) => {
                    subchannels.push(sc);
                }
                other => panic!("unexpected event {:?}", other),
            };
        }
        subchannels
    }

    // State for an individual child in the multi-child aggregate tests.
    #[derive(Default, Debug, Clone)]
    struct ChildTestState {
        received_endpoints: Vec<Endpoint>,
        created_subchannel: Option<Arc<dyn Subchannel>>,
    }
    // Maps an endpoint to the child's ChildTestState
    type AggregateTestState = HashMap<Endpoint, ChildTestState>;

    // Defines the functions resolver_update and subchannel_update
    // to test aggregate_states
    fn create_verifying_funcs_for_aggregate_tests(
        shared_state: Arc<Mutex<AggregateTestState>>,
    ) -> PolicyFuncs {
        PolicyFuncs {
            // Closure for resolver_update. It creates a subchannel
            // for the endpoint it receives and stores which endpoint it received
            // and which subchannel this child created in the data field.
            resolver_update: Some(Arc::new({
                let state_clone = shared_state.clone();
                move |data: &mut Data, update: ResolverUpdate, _, controller| {
                    let endpoint = update.endpoints.as_ref().unwrap()[0].clone();
                    let subchannel = controller.new_subchannel(&endpoint.addresses[0]);

                    // This creates the state for this specific policy instance
                    // with its endpoints and created subchannel.
                    let child_state = ChildTestState {
                        received_endpoints: vec![endpoint.clone()],
                        created_subchannel: Some(subchannel),
                    };

                    // Store this policy's state in its data field.
                    data.test_data = Some(Box::new(child_state.clone()));
                    Ok(())
                }
            })),
            // Closure for subchannel_update. Verify that the subchannel that being updated now is the
            // same one that this child policy created in resolver_update. It then sends
            // a picker of the same state that was passed to it.
            subchannel_update: Some(Arc::new({
                let state_clone_for_update = shared_state.clone();
                move |data: &mut Data, updated_subchannel, state, controller| {
                    // Retrieve the specific ChildTestState from the generic test_data field.
                    // This downcasts the `Any` trait object
                    let test_state = data
                        .test_data
                        .as_ref()
                        .unwrap()
                        .downcast_ref::<ChildTestState>()
                        .unwrap();

                    let created_sc = test_state.created_subchannel.as_ref().unwrap();

                    assert_eq!(
                        created_sc.address().address,
                        updated_subchannel.address().address,
                        "Subchannel update was for the wrong subchannel!"
                    );
                    controller.update_picker(LbState {
                        connectivity_state: state.connectivity_state,
                        picker: Arc::new(DummyPicker::new("dummy")),
                    });
                }
            })),
            ..Default::default()
        }
    }

    // Tests the scenario where one child is READY
    // and the rest are in CONNECTING, IDLE, or TRANSIENT FAILURE.
    // The child manager's aggregate_states function should report READY.
    #[tokio::test]
    async fn childmanager_aggregate_state_is_ready_if_any_child_is_ready() {
        let shared_test_state = Arc::new(Mutex::new(AggregateTestState::default()));
        let (mut rx_events, mut child_manager, mut tcc) = setup(
            create_verifying_funcs_for_aggregate_tests(shared_test_state.clone()),
        );
        let endpoints = create_n_endpoints_with_k_addresses(4, 1);
        send_resolver_update_to_policy(child_manager.as_mut(), endpoints.clone(), tcc.as_mut());
        let mut subchannels = vec![];
        for endpoint in endpoints {
            subchannels.push(
                verify_subchannel_creation_from_policy(&mut rx_events, endpoint.addresses)
                    .await
                    .remove(0),
            );
        }
        move_subchannel_to_transient_failure(
            child_manager.as_mut(),
            subchannels[0].clone(),
            tcc.as_mut(),
        );
        move_subchannel_to_idle(child_manager.as_mut(), subchannels[1].clone(), tcc.as_mut());
        move_subchannel_to_connecting(child_manager.as_mut(), subchannels[2].clone(), tcc.as_mut());
        move_subchannel_to_ready(child_manager.as_mut(), subchannels[3].clone(), tcc.as_mut());
        assert_eq!(child_manager.aggregate_states(), ConnectivityState::Ready);
    }

    // Tests the scenario where no children are READY
    // and the children are in CONNECTING, IDLE, or TRANSIENT FAILURE.
    // The child manager's aggregate_states function should report CONNECTING.
    #[tokio::test]
    async fn childmanager_aggregate_state_is_connecting_if_no_child_is_ready() {
        let shared_test_state = Arc::new(Mutex::new(AggregateTestState::default()));
        let (mut rx_events, mut child_manager, mut tcc) = setup(
            create_verifying_funcs_for_aggregate_tests(shared_test_state.clone()),
        );
        let endpoints = create_n_endpoints_with_k_addresses(3, 1);
        send_resolver_update_to_policy(child_manager.as_mut(), endpoints.clone(), tcc.as_mut());
        let mut subchannels = vec![];
        for endpoint in endpoints {
            subchannels.push(
                verify_subchannel_creation_from_policy(&mut rx_events, endpoint.addresses)
                    .await
                    .remove(0),
            );
        }
        move_subchannel_to_transient_failure(
            child_manager.as_mut(),
            subchannels[0].clone(),
            tcc.as_mut(),
        );
        move_subchannel_to_idle(child_manager.as_mut(), subchannels[1].clone(), tcc.as_mut());
        move_subchannel_to_connecting(child_manager.as_mut(), subchannels[2].clone(), tcc.as_mut());

        assert_eq!(
            child_manager.aggregate_states(),
            ConnectivityState::Connecting
        );
    }

    // Tests the scenario where no children are READY or CONNECTING
    // and the children are in IDLE, or TRANSIENT FAILURE.
    // The child manager's aggregate_states function should report IDLE.
    #[tokio::test]
    async fn childmanager_aggregate_state_is_idle_if_only_idle_and_failure() {
        let shared_test_state = Arc::new(Mutex::new(AggregateTestState::default()));
        let (mut rx_events, mut child_manager, mut tcc) = setup(
            create_verifying_funcs_for_aggregate_tests(shared_test_state.clone()),
        );

        let endpoints = create_n_endpoints_with_k_addresses(2, 1);
        send_resolver_update_to_policy(child_manager.as_mut(), endpoints.clone(), tcc.as_mut());
        let mut subchannels = vec![];
        for endpoint in endpoints {
            subchannels.push(
                verify_subchannel_creation_from_policy(&mut rx_events, endpoint.addresses)
                    .await
                    .remove(0),
            );
        }
        move_subchannel_to_transient_failure(
            child_manager.as_mut(),
            subchannels[0].clone(),
            tcc.as_mut(),
        );
        move_subchannel_to_idle(child_manager.as_mut(), subchannels[1].clone(), tcc.as_mut());
        assert_eq!(child_manager.aggregate_states(), ConnectivityState::Idle);
    }

    // Tests the scenario where no children are READY, CONNECTING, or IDLE
    // and all children are in TRANSIENT FAILURE.
    // The child manager's aggregate_states function should report TRANSIENT FAILURE.
    #[tokio::test]
    async fn childmanager_aggregate_state_is_transient_failure_if_all_children_are() {
        let shared_test_state = Arc::new(Mutex::new(AggregateTestState::default()));
        let (mut rx_events, mut child_manager, mut tcc) = setup(
            create_verifying_funcs_for_aggregate_tests(shared_test_state.clone()),
        );
        let endpoints = create_n_endpoints_with_k_addresses(2, 1);
        send_resolver_update_to_policy(child_manager.as_mut(), endpoints.clone(), tcc.as_mut());
        let mut subchannels = vec![];
        for endpoint in endpoints {
            subchannels.push(
                verify_subchannel_creation_from_policy(&mut rx_events, endpoint.addresses)
                    .await
                    .remove(0),
            );
        }
        move_subchannel_to_transient_failure(
            child_manager.as_mut(),
            subchannels[0].clone(),
            tcc.as_mut(),
        );
        move_subchannel_to_transient_failure(
            child_manager.as_mut(),
            subchannels[1].clone(),
            tcc.as_mut(),
        );
        assert_eq!(
            child_manager.aggregate_states(),
            ConnectivityState::TransientFailure
        );
    }
}
