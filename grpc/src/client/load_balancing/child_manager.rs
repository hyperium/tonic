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
use std::fmt::Debug;
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
#[derive(Debug)]
pub(crate) struct ChildManager<T: Debug, S: ResolverUpdateSharder<T>> {
    subchannel_child_map: HashMap<WeakSubchannel, usize>,
    children: Vec<Child<T>>,
    update_sharder: S,
    pending_work: Arc<Mutex<HashSet<usize>>>,
    runtime: Arc<dyn Runtime>,
    updated: bool, // Set when any child updates its picker; cleared when accessed.
}

#[non_exhaustive]
#[derive(Debug)]
pub(crate) struct Child<T> {
    pub identifier: T,
    pub policy: Box<dyn LbPolicy>,
    pub builder: Arc<dyn LbPolicyBuilder>,
    pub state: LbState,
    pub updated: bool, // Set when the child updates its picker; cleared in child_states is called.
    work_scheduler: Arc<ChildWorkScheduler>,
}

/// A collection of data sent to a child of the ChildManager.
pub(crate) struct ChildUpdate<T> {
    /// The identifier the ChildManager should use for this child.
    pub child_identifier: T,
    /// The builder the ChildManager should use to create this child if it does
    /// not exist.  The child_policy_builder's name is effectively a part of the
    /// child_identifier.  If two identifiers are identical but have different
    /// builder names, they are treated as different children.
    pub child_policy_builder: Arc<dyn LbPolicyBuilder>,
    /// The relevant ResolverUpdate and LbConfig to send to this child.  If
    /// None, then resolver_update will not be called on the child.  Should
    /// generally be Some for any new children, otherwise they will not be
    /// called.
    pub child_update: Option<(ResolverUpdate, Option<LbConfig>)>,
}

pub(crate) trait ResolverUpdateSharder<T>: Send {
    /// Performs the operation of sharding an aggregate ResolverUpdate/LbConfig
    /// into one or more ChildUpdates.  Called automatically by the ChildManager
    /// when its resolver_update method is called.
    fn shard_update(
        &mut self,
        update: ResolverUpdate,
        config: Option<&LbConfig>,
    ) -> Result<impl Iterator<Item = ChildUpdate<T>>, Box<dyn Error + Send + Sync>>;
}

impl<T: Debug, S> ChildManager<T, S>
where
    S: ResolverUpdateSharder<T>,
{
    /// Creates a new ChildManager LB policy.  shard_update is called whenever a
    /// resolver_update operation occurs.
    pub fn new(update_sharder: S, runtime: Arc<dyn Runtime>) -> Self {
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
    pub fn children(&self) -> impl Iterator<Item = &Child<T>> {
        self.children.iter()
    }

    /// Aggregates states from child policies.
    ///
    /// If any child is READY then we consider the aggregate state to be READY.
    /// Otherwise, if any child is CONNECTING, then report CONNECTING.
    /// Otherwise, if any child is IDLE, then report IDLE.
    /// Report TRANSIENT FAILURE if no conditions above apply.
    pub fn aggregate_states(&self) -> ConnectivityState {
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
            self.children[child_idx].updated = true;
            self.updated = true;
        };
    }

    /// Returns a mutable reference to the update sharder so operations may be
    /// performed on it for instances in which it needs to retain state.
    pub fn update_sharder(&mut self) -> &mut S {
        &mut self.update_sharder
    }

    /// Returns true if any child has updated its picker since the last call to
    /// child_updated.
    pub fn child_updated(&mut self) -> bool {
        mem::take(&mut self.updated)
    }
}

impl<T: Debug, S: Debug> LbPolicy for ChildManager<T, S>
where
    T: PartialEq + Hash + Eq + Send + Sync + 'static,
    S: ResolverUpdateSharder<T>,
{
    fn resolver_update(
        &mut self,
        resolver_update: ResolverUpdate,
        config: Option<&LbConfig>,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // First determine if the incoming update is valid.
        let child_updates = self.update_sharder.shard_update(resolver_update, config)?;

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
        // This leverages a Child<usize> to hold all the entries where the
        // identifier becomes the index within the old self.children vector.
        let old_children = old_children.into_iter().enumerate().map(|(old_idx, e)| {
            (
                (e.builder.name(), e.identifier),
                Child {
                    identifier: old_idx,
                    policy: e.policy,
                    builder: e.builder,
                    state: e.state,
                    updated: e.updated,
                    work_scheduler: e.work_scheduler,
                },
            )
        });
        let mut old_children: HashMap<(&'static str, T), _> = old_children.collect();

        // Split the child updates into the IDs and builders, and the
        // ResolverUpdates/LbConfigs.
        let (ids_builders, updates): (Vec<_>, Vec<_>) = child_updates
            .map(|e| ((e.child_identifier, e.child_policy_builder), e.child_update))
            .unzip();

        // Transfer children whose identifiers appear before and after the
        // update, and create new children.  Add entries back into the
        // subchannel map.
        for (new_idx, (identifier, builder)) in ids_builders.into_iter().enumerate() {
            let k = (builder.name(), identifier);
            if let Some(old_child) = old_children.remove(&k) {
                for subchannel in old_child_subchannels_map
                    .remove(&old_child.identifier)
                    .into_iter()
                    .flatten()
                {
                    self.subchannel_child_map.insert(subchannel, new_idx);
                }
                if old_pending_work.contains(&old_child.identifier) {
                    pending_work.insert(new_idx);
                }
                *old_child.work_scheduler.idx.lock().unwrap() = Some(new_idx);
                self.children.push(Child {
                    builder,
                    identifier: k.1,
                    state: old_child.state,
                    policy: old_child.policy,
                    work_scheduler: old_child.work_scheduler,
                    updated: old_child.updated,
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
                self.children.push(Child {
                    builder,
                    identifier: k.1,
                    state: LbState::initial(),
                    policy,
                    work_scheduler,
                    updated: false,
                });
            };
        }

        // Invalidate all deleted children's work_schedulers.
        for (_, old_child) in old_children {
            *old_child.work_scheduler.idx.lock().unwrap() = None;
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
            let Some((resolver_update, config)) = child_update else {
                continue;
            };
            let mut channel_controller = WrappedController::new(channel_controller);
            let _ = child.policy.resolver_update(
                resolver_update,
                config.as_ref(),
                &mut channel_controller,
            );
            self.resolve_child_controller(channel_controller, child_idx);
        }
        Ok(())
    }

    // Forwards the subchannel_update to the child that created the subchannel
    // being updated.
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

    // Calls work on any children that scheduled work via our work scheduler.
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

    // Simply calls exit_idle on all children.
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

#[derive(Debug)]
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
        ChildManager, ChildUpdate, ResolverUpdateSharder,
    };
    use crate::client::load_balancing::test_utils::{
        self, StubPolicyData, StubPolicyFuncs, TestChannelController, TestEvent,
    };
    use crate::client::load_balancing::{
        ChannelController, LbPolicy, LbPolicyBuilder, LbState, QueuingPicker, Subchannel,
        SubchannelState, GLOBAL_LB_REGISTRY,
    };
    use crate::client::name_resolution::{Address, Endpoint, ResolverUpdate};
    use crate::client::service_config::LbConfig;
    use crate::client::ConnectivityState;
    use crate::rt::default_runtime;
    use std::error::Error;
    use std::panic;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    // TODO: This needs to be moved to a common place that can be shared between
    // round_robin and this test. This EndpointSharder maps endpoints to
    // children policies.
    #[derive(Debug)]
    struct EndpointSharder {
        builder: Arc<dyn LbPolicyBuilder>,
    }

    impl ResolverUpdateSharder<Endpoint> for EndpointSharder {
        fn shard_update(
            &mut self,
            resolver_update: ResolverUpdate,
            config: Option<&LbConfig>,
        ) -> Result<impl Iterator<Item = ChildUpdate<Endpoint>>, Box<dyn Error + Send + Sync>>
        {
            let mut sharded_endpoints = Vec::new();
            for endpoint in resolver_update.endpoints.unwrap().into_iter() {
                let child_update = ChildUpdate {
                    child_identifier: endpoint.clone(),
                    child_policy_builder: self.builder.clone(),
                    child_update: Some((
                        ResolverUpdate {
                            attributes: resolver_update.attributes.clone(),
                            endpoints: Ok(vec![endpoint]),
                            service_config: resolver_update.service_config.clone(),
                            resolution_note: resolver_update.resolution_note.clone(),
                        },
                        config.cloned(),
                    )),
                };
                sharded_endpoints.push(child_update);
            }
            Ok(sharded_endpoints.into_iter())
        }
    }

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
        ChildManager<Endpoint, EndpointSharder>,
        Box<dyn ChannelController>,
    ) {
        test_utils::reg_stub_policy(test_name, funcs);
        let (tx_events, rx_events) = mpsc::unbounded_channel::<TestEvent>();
        let tcc = Box::new(TestChannelController { tx_events });
        let builder: Arc<dyn LbPolicyBuilder> = GLOBAL_LB_REGISTRY.get_policy(test_name).unwrap();
        let endpoint_sharder = EndpointSharder { builder };
        let child_manager = ChildManager::new(endpoint_sharder, default_runtime());
        (rx_events, child_manager, tcc)
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
        lb_policy: &mut impl LbPolicy,
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
        let data = StubPolicyData::new();
        StubPolicyFuncs {
            // Closure for resolver_update. resolver_update should only receive
            // one endpoint and create one subchannel for the endpoint it
            // receives.
            resolver_update: Some(Arc::new(
                move |data, update: ResolverUpdate, _, controller| {
                    assert_eq!(update.endpoints.iter().len(), 1);
                    let endpoint = update.endpoints.unwrap().pop().unwrap();
                    let subchannel = controller.new_subchannel(&endpoint.addresses[0]);
                    Ok(())
                },
            )),
            // Closure for subchannel_update. Sends a picker of the same state
            // that was passed to it.
            subchannel_update: Some(Arc::new(
                move |data, updated_subchannel, state, controller| {
                    controller.update_picker(LbState {
                        connectivity_state: state.connectivity_state,
                        picker: Arc::new(QueuingPicker {}),
                    });
                },
            )),
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
        send_resolver_update_to_policy(&mut child_manager, endpoints.clone(), tcc.as_mut());
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
            &mut child_manager,
            subchannels.next().unwrap(),
            tcc.as_mut(),
            ConnectivityState::TransientFailure,
        );
        move_subchannel_to_state(
            &mut child_manager,
            subchannels.next().unwrap(),
            tcc.as_mut(),
            ConnectivityState::Idle,
        );
        move_subchannel_to_state(
            &mut child_manager,
            subchannels.next().unwrap(),
            tcc.as_mut(),
            ConnectivityState::Connecting,
        );
        move_subchannel_to_state(
            &mut child_manager,
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
        send_resolver_update_to_policy(&mut child_manager, endpoints.clone(), tcc.as_mut());
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
            &mut child_manager,
            subchannels.next().unwrap(),
            tcc.as_mut(),
            ConnectivityState::TransientFailure,
        );
        move_subchannel_to_state(
            &mut child_manager,
            subchannels.next().unwrap(),
            tcc.as_mut(),
            ConnectivityState::Idle,
        );
        move_subchannel_to_state(
            &mut child_manager,
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
        send_resolver_update_to_policy(&mut child_manager, endpoints.clone(), tcc.as_mut());
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
            &mut child_manager,
            subchannels.next().unwrap(),
            tcc.as_mut(),
            ConnectivityState::TransientFailure,
        );
        move_subchannel_to_state(
            &mut child_manager,
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
        send_resolver_update_to_policy(&mut child_manager, endpoints.clone(), tcc.as_mut());
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
            &mut child_manager,
            subchannels.next().unwrap(),
            tcc.as_mut(),
            ConnectivityState::TransientFailure,
        );
        move_subchannel_to_state(
            &mut child_manager,
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
