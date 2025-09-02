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
use crate::client::name_resolution::{Address, Endpoint, ResolverUpdate};
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
    updated: bool,
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

/// EndpointSharder shards a resolver update into individual endpoints,
/// with each endpoint serving as the unique identifier for a child.
///
/// The EndpointSharder implements the ResolverUpdateSharder trait,
/// allowing any load-balancing (LB) policy that uses the ChildManager
/// to split a resolver update into individual endpoints, with one endpoint for each child.
pub struct EndpointSharder {
    pub builder: Arc<dyn LbPolicyBuilder>,
}

// Creates a ChildUpdate for each endpoint received.
impl ResolverUpdateSharder<Endpoint> for EndpointSharder {
    fn shard_update(
        &self,
        resolver_update: ResolverUpdate,
    ) -> Result<Box<dyn Iterator<Item = ChildUpdate<Endpoint>>>, Box<dyn Error + Send + Sync>> {
        let update: Vec<_> = resolver_update
            .endpoints
            .unwrap()
            .into_iter()
            .map(|e| ChildUpdate {
                child_identifier: e.clone(),
                child_policy_builder: self.builder.clone(),
                child_update: ResolverUpdate {
                    attributes: resolver_update.attributes.clone(),
                    endpoints: Ok(vec![e.clone()]),
                    service_config: resolver_update.service_config.clone(),
                    resolution_note: resolver_update.resolution_note.clone(),
                },
            })
            .collect();
        Ok(Box::new(update.into_iter()))
    }
}

impl EndpointSharder {
    pub fn new(builder: Arc<dyn LbPolicyBuilder>) -> Self {
        Self { builder }
    }
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
            updated: false,
        }
    }

    /// Returns data for all current children.
    pub fn child_states(&mut self) -> impl Iterator<Item = (&T, &LbState)> {
        self.children
            .iter()
            .map(|child| (&child.identifier, &child.state))
    }

    /// Aggregates states from child policies.
    ///
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
            self.updated = true;
        };
    }

    // Called to update all accounting in the ChildManager from operations
    // performed by a child policy on the WrappedController that was created for
    // it.  child_idx is an index into the children map for the relevant child.
    //
    // TODO: this post-processing step can be eliminated by capturing the right
    // state inside the WrappedController, however it is fairly complex.  Decide
    // which way is better.
    pub(crate) fn forward_update_to_children(
        &mut self,
        channel_controller: &mut dyn ChannelController,
        resolver_update: ResolverUpdate,
        config: Option<&LbConfig>,
    ) {
        for child_idx in 0..self.children.len() {
            let child = &mut self.children[child_idx];
            let mut channel_controller = WrappedController::new(channel_controller);
            let _ = child.policy.resolver_update(
                resolver_update.clone(),
                config,
                &mut channel_controller,
            );
            self.resolve_child_controller(channel_controller, child_idx);
        }
    }

    /// Checks whether a child has produced an update.
    pub fn has_updated(&mut self) -> bool {
        mem::take(&mut self.updated)
    }

    /// Returns true if ChildManager has children.
    pub fn has_children(&self) -> bool {
        !self.children.is_empty()
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

    fn exit_idle(&mut self, channel_controller: &mut dyn ChannelController) {
        for child_idx in 0..self.children.len() {
            let child = &mut self.children[child_idx];
            let mut channel_controller = WrappedController::new(channel_controller);
            child.policy.exit_idle(&mut channel_controller);
            self.resolve_child_controller(channel_controller, child_idx);
        }
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
        Child, ChildManager, ChildUpdate, ChildWorkScheduler, EndpointSharder,
        ResolverUpdateSharder,
    };
    use crate::client::load_balancing::test_utils::{
        self, StubPolicy, StubPolicyData, StubPolicyFuncs, TestChannelController, TestEvent,
        TestSubchannel, TestWorkScheduler,
    };
    use crate::client::load_balancing::{
        ChannelController, LbPolicy, LbPolicyBuilder, LbPolicyOptions, LbState, ParsedJsonLbConfig,
        Pick, PickResult, Picker, QueuingPicker, Subchannel, SubchannelState, GLOBAL_LB_REGISTRY,
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
    // 3. Creates an StubPolicyBuilder with StubFuncs that each test will define
    //    and name of the test.
    // 4. Creates an EndpointSharder with StubPolicyBuilder passed in as the
    //    child policy.
    // 5. Creates a ChildManager with the EndpointSharder.
    //
    // Returns the following:
    // 1. A receiver for events initiated by the LB policy (like creating a new
    //    subchannel, sending a new picker etc).
    // 2. The ChildManager to send resolver and subchannel updates from the
    //    test.
    // 3. The controller to pass to the LB policy as part of the updates.
    fn setup(
        funcs: StubPolicyFuncs,
        test_name: &'static str,
    ) -> (
        mpsc::UnboundedReceiver<TestEvent>,
        Box<ChildManager<Endpoint>>,
        Box<dyn ChannelController>,
    ) {
        test_utils::reg_stub_policy(test_name, funcs);
        let (tx_events, rx_events) = mpsc::unbounded_channel::<TestEvent>();
        let tcc = Box::new(TestChannelController { tx_events });
        let builder: Arc<dyn LbPolicyBuilder> = GLOBAL_LB_REGISTRY.get_policy(test_name).unwrap();
        let endpoint_sharder = EndpointSharder::new(builder);
        let child_manager = ChildManager::new(Box::new(endpoint_sharder), default_runtime());
        (rx_events, Box::new(child_manager), tcc)
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
        assert!(lb_policy.resolver_update(update, None, tcc).is_ok());
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

    // Defines the functions resolver_update and subchannel_update to test
    // aggregate_states.
    fn create_verifying_funcs_for_aggregate_tests() -> StubPolicyFuncs {
        let data = StubPolicyData::default();
        StubPolicyFuncs {
            // Closure for resolver_update. resolver_update should only receive
            // one endpoint and create one subchannel for the endpoint it
            // receives.
            resolver_update: Some(move |data, update: ResolverUpdate, _, controller| {
                assert_eq!(update.endpoints.iter().len(), 1);
                let endpoint = update.endpoints.unwrap().pop().unwrap();
                let subchannel = controller.new_subchannel(&endpoint.addresses[0]);
                Ok(())
            }),
            // Closure for subchannel_update. Sends a picker of the same state
            // that was passed to it.
            subchannel_update: Some(move |data, updated_subchannel, state, controller| {
                controller.update_picker(LbState {
                    connectivity_state: state.connectivity_state,
                    picker: Arc::new(QueuingPicker {}),
                });
            }),
            ..Default::default()
        }
    }

    // Tests the scenario where one child is READY and the rest are in
    // CONNECTING, IDLE, or TRANSIENT FAILURE. The child manager's
    // aggregate_states function should report READY.
    #[tokio::test]
    async fn childmanager_aggregate_state_is_ready_if_any_child_is_ready() {
        let (mut rx_events, mut child_manager, mut tcc) = setup(
            create_verifying_funcs_for_aggregate_tests(),
            "stub-childmanager_aggregate_state_is_ready_if_any_child_is_ready",
        );
        let endpoints = create_n_endpoints_with_k_addresses(4, 1);
        send_resolver_update_to_policy(child_manager.as_mut(), endpoints.clone(), tcc.as_mut());
        let mut subchannels = vec![];
        for endpoint in endpoints {
            subchannels.push(
                verify_subchannel_creation_from_policy(&mut rx_events, endpoint.addresses.len())
                    .await
                    .remove(0),
            );
        }

        let mut subchannels = subchannels.into_iter();
        move_subchannel_to_state(
            child_manager.as_mut(),
            subchannels.next().unwrap(),
            tcc.as_mut(),
            ConnectivityState::TransientFailure,
        );
        move_subchannel_to_state(
            child_manager.as_mut(),
            subchannels.next().unwrap(),
            tcc.as_mut(),
            ConnectivityState::Idle,
        );
        move_subchannel_to_state(
            child_manager.as_mut(),
            subchannels.next().unwrap(),
            tcc.as_mut(),
            ConnectivityState::Connecting,
        );
        move_subchannel_to_state(
            child_manager.as_mut(),
            subchannels.next().unwrap(),
            tcc.as_mut(),
            ConnectivityState::Ready,
        );
        assert_eq!(child_manager.aggregate_states(), ConnectivityState::Ready);
    }

    // Tests the scenario where no children are READY and the children are in
    // CONNECTING, IDLE, or TRANSIENT FAILURE. The child manager's
    // aggregate_states function should report CONNECTING.
    #[tokio::test]
    async fn childmanager_aggregate_state_is_connecting_if_no_child_is_ready() {
        let (mut rx_events, mut child_manager, mut tcc) = setup(
            create_verifying_funcs_for_aggregate_tests(),
            "stub-childmanager_aggregate_state_is_connecting_if_no_child_is_ready",
        );
        let endpoints = create_n_endpoints_with_k_addresses(3, 1);
        send_resolver_update_to_policy(child_manager.as_mut(), endpoints.clone(), tcc.as_mut());
        let mut subchannels = vec![];
        for endpoint in endpoints {
            subchannels.push(
                verify_subchannel_creation_from_policy(&mut rx_events, endpoint.addresses.len())
                    .await
                    .remove(0),
            );
        }
        let mut subchannels = subchannels.into_iter();
        move_subchannel_to_state(
            child_manager.as_mut(),
            subchannels.next().unwrap(),
            tcc.as_mut(),
            ConnectivityState::TransientFailure,
        );
        move_subchannel_to_state(
            child_manager.as_mut(),
            subchannels.next().unwrap(),
            tcc.as_mut(),
            ConnectivityState::Idle,
        );
        move_subchannel_to_state(
            child_manager.as_mut(),
            subchannels.next().unwrap(),
            tcc.as_mut(),
            ConnectivityState::Connecting,
        );

        assert_eq!(
            child_manager.aggregate_states(),
            ConnectivityState::Connecting
        );
    }

    // Tests the scenario where no children are READY or CONNECTING and the
    // children are in IDLE, or TRANSIENT FAILURE. The child manager's
    // aggregate_states function should report IDLE.
    #[tokio::test]
    async fn childmanager_aggregate_state_is_idle_if_only_idle_and_failure() {
        let (mut rx_events, mut child_manager, mut tcc) = setup(
            create_verifying_funcs_for_aggregate_tests(),
            "stub-childmanager_aggregate_state_is_idle_if_only_idle_and_failure",
        );

        let endpoints = create_n_endpoints_with_k_addresses(2, 1);
        send_resolver_update_to_policy(child_manager.as_mut(), endpoints.clone(), tcc.as_mut());
        let mut subchannels = vec![];
        for endpoint in endpoints {
            subchannels.push(
                verify_subchannel_creation_from_policy(&mut rx_events, endpoint.addresses.len())
                    .await
                    .remove(0),
            );
        }
        let mut subchannels = subchannels.into_iter();
        move_subchannel_to_state(
            child_manager.as_mut(),
            subchannels.next().unwrap(),
            tcc.as_mut(),
            ConnectivityState::TransientFailure,
        );
        move_subchannel_to_state(
            child_manager.as_mut(),
            subchannels.next().unwrap(),
            tcc.as_mut(),
            ConnectivityState::Idle,
        );
        assert_eq!(child_manager.aggregate_states(), ConnectivityState::Idle);
    }

    // Tests the scenario where no children are READY, CONNECTING, or IDLE and
    // all children are in TRANSIENT FAILURE. The child manager's
    // aggregate_states function should report TRANSIENT FAILURE.
    #[tokio::test]
    async fn childmanager_aggregate_state_is_transient_failure_if_all_children_are() {
        let (mut rx_events, mut child_manager, mut tcc) = setup(
            create_verifying_funcs_for_aggregate_tests(),
            "stub-childmanager_aggregate_state_is_transient_failure_if_all_children_are",
        );
        let endpoints = create_n_endpoints_with_k_addresses(2, 1);
        send_resolver_update_to_policy(child_manager.as_mut(), endpoints.clone(), tcc.as_mut());
        let mut subchannels = vec![];
        for endpoint in endpoints {
            subchannels.push(
                verify_subchannel_creation_from_policy(&mut rx_events, endpoint.addresses.len())
                    .await
                    .remove(0),
            );
        }
        let mut subchannels = subchannels.into_iter();
        move_subchannel_to_state(
            child_manager.as_mut(),
            subchannels.next().unwrap(),
            tcc.as_mut(),
            ConnectivityState::TransientFailure,
        );
        move_subchannel_to_state(
            child_manager.as_mut(),
            subchannels.next().unwrap(),
            tcc.as_mut(),
            ConnectivityState::TransientFailure,
        );
        assert_eq!(
            child_manager.aggregate_states(),
            ConnectivityState::TransientFailure
        );
    }
}
