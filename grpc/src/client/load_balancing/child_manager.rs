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
// production.

use std::collections::HashMap;
use std::collections::HashSet;
use std::error::Error;
use std::fmt::Debug;
use std::hash::Hash;
use std::mem;
use std::sync::Arc;
use std::sync::Mutex;

use crate::client::ConnectivityState;
use crate::client::load_balancing::ChannelController;
use crate::client::load_balancing::LbConfig;
use crate::client::load_balancing::LbPolicy;
use crate::client::load_balancing::LbPolicyBuilder;
use crate::client::load_balancing::LbPolicyOptions;
use crate::client::load_balancing::LbState;
use crate::client::load_balancing::Subchannel;
use crate::client::load_balancing::SubchannelState;
use crate::client::load_balancing::WeakSubchannel;
use crate::client::load_balancing::WorkScheduler;
use crate::client::name_resolution::Address;
use crate::client::name_resolution::ResolverUpdate;
use crate::rt::GrpcRuntime;

// An LbPolicy implementation that manages multiple children.
#[derive(Debug)]
pub(crate) struct ChildManager<T: Debug> {
    subchannel_to_child_idx: HashMap<WeakSubchannel, usize>,
    children: Vec<Child<T>>,
    pending_work: Arc<Mutex<HashSet<usize>>>,
    runtime: GrpcRuntime,
    updated: bool, // Set when any child updates its picker; cleared when accessed.
    work_scheduler: Arc<dyn WorkScheduler>,
}

#[non_exhaustive]
#[derive(Debug)]
pub(crate) struct Child<T> {
    pub identifier: T,
    pub builder: Arc<dyn LbPolicyBuilder>,
    pub state: LbState,
    policy: Box<dyn LbPolicy>,
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

impl<T> ChildManager<T>
where
    T: Debug + PartialEq + Hash + Eq + Send + Sync + 'static,
{
    /// Creates a new ChildManager LB policy.  shard_update is called whenever a
    /// resolver_update operation occurs.
    pub fn new(runtime: GrpcRuntime, work_scheduler: Arc<dyn WorkScheduler>) -> Self {
        Self {
            subchannel_to_child_idx: Default::default(),
            children: Default::default(),
            pending_work: Default::default(),
            runtime,
            work_scheduler,
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
            self.subchannel_to_child_idx.insert(csc.into(), child_idx);
        }
        // Update the tracked state if the child produced an update.
        if let Some(state) = channel_controller.picker_update {
            self.children[child_idx].state = state;
            self.updated = true;
        };
    }

    /// Returns true if any child has updated its picker since the last call to
    /// child_updated.
    pub fn child_updated(&mut self) -> bool {
        mem::take(&mut self.updated)
    }

    /// Retains only the child policies specified by the iterator.
    ///
    /// If an ID is provided that does not exist in the ChildManager, it will be
    /// ignored.
    pub fn retain_children(
        &mut self,
        ids_builders: impl IntoIterator<Item = (T, Arc<dyn LbPolicyBuilder>)>,
    ) {
        self.reset_children(ids_builders, true);
    }

    /// Resets the children and all state related to tracking them in accordance
    /// with the iterator provided.  When retain_only is true, any entry in
    /// ids_builders that is not in the current set of children will be ignored;
    /// otherwise a new child will be built for it.
    fn reset_children(
        &mut self,
        ids_builders: impl IntoIterator<Item = (T, Arc<dyn LbPolicyBuilder>)>,
        retain_only: bool,
    ) {
        // Hold the lock to prevent new work requests during this operation and
        // rewrite the indices.
        let mut pending_work = self.pending_work.lock().unwrap();

        // Reset pending work; we will re-add any entries it contains with the
        // right index later.
        let old_pending_work = mem::take(&mut *pending_work);

        // Replace self.children with an empty vec.
        let old_children = mem::take(&mut self.children);

        // Replace the subchannel map with an empty map.
        let old_subchannel_child_map = mem::take(&mut self.subchannel_to_child_idx);

        // Reverse the old subchannel map into a vector indexed by the old child ID.
        let mut old_child_subchannels: Vec<Vec<WeakSubchannel>> = Vec::new();
        old_child_subchannels.resize_with(old_children.len(), Vec::new);

        for (subchannel, old_idx) in old_subchannel_child_map {
            old_child_subchannels[old_idx].push(subchannel);
        }

        // Build a map of the old children from their IDs for efficient lookups.
        // This leverages a Child<usize> to hold all the entries where the
        // identifier becomes the index within the old self.children vector.
        let mut old_children: HashMap<(&'static str, T), _> = old_children
            .into_iter()
            .enumerate()
            .map(|(old_idx, e)| {
                (
                    (e.builder.name(), e.identifier),
                    Child {
                        identifier: old_idx,
                        policy: e.policy,
                        builder: e.builder,
                        state: e.state,
                        work_scheduler: e.work_scheduler,
                    },
                )
            })
            .collect();

        // Transfer children whose identifiers appear before and after the
        // update, and create new children.  Add entries back into the
        // subchannel map.
        for (new_idx, (identifier, builder)) in ids_builders.into_iter().enumerate() {
            let k = (builder.name(), identifier);
            if let Some(old_child) = old_children.remove(&k) {
                let old_idx = old_child.identifier;
                for subchannel in mem::take(&mut old_child_subchannels[old_idx]) {
                    self.subchannel_to_child_idx.insert(subchannel, new_idx);
                }
                if old_pending_work.contains(&old_idx) {
                    pending_work.insert(new_idx);
                }
                *old_child.work_scheduler.idx.lock().unwrap() = Some(new_idx);
                self.children.push(Child {
                    builder,
                    identifier: k.1,
                    state: old_child.state,
                    policy: old_child.policy,
                    work_scheduler: old_child.work_scheduler,
                });
            } else if !retain_only {
                let work_scheduler = Arc::new(ChildWorkScheduler {
                    pending_work: self.pending_work.clone(),
                    idx: Mutex::new(Some(new_idx)),
                    work_scheduler: self.work_scheduler.clone(),
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
                });
            };
        }

        // Invalidate all deleted children's work_schedulers.
        for (_, old_child) in old_children {
            old_child.work_scheduler.invalidate();
        }
        // Anything left in old_children will just be Dropped and cleaned up.
    }

    /// Updates the ChildManager's children.
    ///
    /// `child_updates` is used to determine which children should exist (one
    /// for each item), how to construct them if they don't already, and what to
    /// send to their `resolver_update` methods, if anything.  Any existing
    /// children not present in child_updates will be removed.
    pub fn update(
        &mut self,
        child_updates: impl IntoIterator<Item = ChildUpdate<T>>,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        // Split the child updates into the IDs and builders, and the
        // ResolverUpdates/LbConfigs.
        let mut errs = vec![];
        let (ids_builders, updates): (Vec<_>, Vec<_>) = child_updates
            .into_iter()
            .map(|e| ((e.child_identifier, e.child_policy_builder), e.child_update))
            .unzip();

        self.reset_children(ids_builders, false);

        // Call resolver_update on all children.
        let mut updates = updates.into_iter();
        for child_idx in 0..self.children.len() {
            let child = &mut self.children[child_idx];
            let child_update = updates.next().unwrap();
            let Some((resolver_update, config)) = child_update else {
                continue;
            };
            let mut channel_controller = WrappedController::new(channel_controller);
            if let Err(err) = child.policy.resolver_update(
                resolver_update,
                config.as_ref(),
                &mut channel_controller,
            ) {
                errs.push(err);
            }
            self.resolve_child_controller(channel_controller, child_idx);
        }
        if errs.is_empty() {
            Ok(())
        } else {
            let err = errs
                .into_iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            Err(err.into())
        }
    }

    /// Forwards the `resolver_update` and `config` to all current children.
    ///
    /// Returns the Result from calling into each child.
    pub fn resolver_update(
        &mut self,
        resolver_update: ResolverUpdate,
        config: Option<&LbConfig>,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let mut errs = Vec::with_capacity(self.children.len());
        for child_idx in 0..self.children.len() {
            let child = &mut self.children[child_idx];
            let mut channel_controller = WrappedController::new(channel_controller);
            if let Err(err) = child.policy.resolver_update(
                resolver_update.clone(),
                config,
                &mut channel_controller,
            ) {
                errs.push(err);
            }
            self.resolve_child_controller(channel_controller, child_idx);
        }
        if errs.is_empty() {
            Ok(())
        } else {
            let err = errs
                .into_iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            Err(err.into())
        }
    }

    /// Forwards the incoming subchannel_update to the child that created the
    /// subchannel being updated.
    pub fn subchannel_update(
        &mut self,
        subchannel: Arc<dyn Subchannel>,
        state: &SubchannelState,
        channel_controller: &mut dyn ChannelController,
    ) {
        // Determine which child created this subchannel.
        let child_idx = *self
            .subchannel_to_child_idx
            .get(&WeakSubchannel::new(&subchannel))
            .unwrap();
        let policy = &mut self.children[child_idx].policy;
        // Wrap the channel_controller to track the child's operations.
        let mut channel_controller = WrappedController::new(channel_controller);
        // Call the proper child.
        policy.subchannel_update(subchannel, state, &mut channel_controller);
        self.resolve_child_controller(channel_controller, child_idx);
    }

    /// Calls work on any children that scheduled work via the work scheduler.
    pub fn work(&mut self, channel_controller: &mut dyn ChannelController) {
        let child_idxes = mem::take(&mut *self.pending_work.lock().unwrap());
        for child_idx in child_idxes {
            let mut channel_controller = WrappedController::new(channel_controller);
            self.children[child_idx]
                .policy
                .work(&mut channel_controller);
            self.resolve_child_controller(channel_controller, child_idx);
        }
    }

    /// Calls exit_idle on all children.
    pub fn exit_idle(&mut self, channel_controller: &mut dyn ChannelController) {
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
    work_scheduler: Arc<dyn WorkScheduler>, // The real work scheduler of the channel.
    pending_work: Arc<Mutex<HashSet<usize>>>, // Must be taken first for correctness
    idx: Mutex<Option<usize>>,              // None if the child is deleted.
}

impl WorkScheduler for ChildWorkScheduler {
    fn schedule_work(&self) {
        let mut pending_work = self.pending_work.lock().unwrap();
        // If self.idx is None then this WorkScheduler has been invalidated as
        // it is associated with a deleted child; do nothing in that case.
        if let Some(idx) = *self.idx.lock().unwrap() {
            pending_work.insert(idx);
            self.work_scheduler.schedule_work();
        }
    }
}

impl ChildWorkScheduler {
    // Sets the ChildWorkScheduler so that it will not honor future
    // schedule_work requests.
    fn invalidate(&self) {
        *self.idx.lock().unwrap() = None;
    }
}

#[cfg(test)]
mod test {
    use crate::client::ConnectivityState;
    use crate::client::load_balancing::ChannelController;
    use crate::client::load_balancing::GLOBAL_LB_REGISTRY;
    use crate::client::load_balancing::LbPolicyBuilder;
    use crate::client::load_balancing::LbState;
    use crate::client::load_balancing::QueuingPicker;
    use crate::client::load_balancing::Subchannel;
    use crate::client::load_balancing::SubchannelState;
    use crate::client::load_balancing::child_manager::ChildManager;
    use crate::client::load_balancing::child_manager::ChildUpdate;
    use crate::client::load_balancing::test_utils::StubPolicyFuncs;
    use crate::client::load_balancing::test_utils::TestChannelController;
    use crate::client::load_balancing::test_utils::TestEvent;
    use crate::client::load_balancing::test_utils::TestWorkScheduler;
    use crate::client::load_balancing::test_utils::{self};
    use crate::client::name_resolution::Address;
    use crate::client::name_resolution::Endpoint;
    use crate::client::name_resolution::ResolverUpdate;
    use crate::client::service_config::LbConfig;
    use crate::rt::default_runtime;
    use std::collections::HashMap;
    use std::error::Error;
    use std::panic;
    use std::sync::Arc;
    use std::sync::Mutex;
    use tokio::sync::mpsc;

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
        ChildManager<Endpoint>,
        Box<dyn ChannelController>,
    ) {
        test_utils::reg_stub_policy(test_name, funcs);
        let (tx_events, rx_events) = mpsc::unbounded_channel::<TestEvent>();
        let tcc = Box::new(TestChannelController {
            tx_events: tx_events.clone(),
        });
        let child_manager =
            ChildManager::new(default_runtime(), Arc::new(TestWorkScheduler { tx_events }));
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
        child_manager: &mut ChildManager<Endpoint>,
        endpoints: Vec<Endpoint>,
        builder: Arc<dyn LbPolicyBuilder>,
        tcc: &mut dyn ChannelController,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let updates = endpoints.iter().map(|e| ChildUpdate {
            child_identifier: e.clone(),
            child_policy_builder: builder.clone(),
            child_update: Some((
                ResolverUpdate {
                    attributes: crate::attributes::Attributes::default(),
                    endpoints: Ok(vec![e.clone()]),
                    service_config: Ok(None),
                    resolution_note: None,
                },
                None,
            )),
        });

        child_manager.update(updates, tcc)
    }

    fn move_subchannel_to_state(
        child_manager: &mut ChildManager<Endpoint>,
        subchannel: Arc<dyn Subchannel>,
        tcc: &mut dyn ChannelController,
        state: ConnectivityState,
    ) {
        child_manager.subchannel_update(
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
            work: None,
        }
    }

    // Tests the scenario where one child is READY and the rest are in
    // CONNECTING, IDLE, or TRANSIENT FAILURE. The child manager's
    // aggregate_states function should report READY.
    #[tokio::test]
    async fn childmanager_aggregate_state_is_ready_if_any_child_is_ready() {
        let test_name = "stub-childmanager_aggregate_state_is_ready_if_any_child_is_ready";
        let (mut rx_events, mut child_manager, mut tcc) =
            setup(create_verifying_funcs_for_aggregate_tests(), test_name);
        let builder: Arc<dyn LbPolicyBuilder> = GLOBAL_LB_REGISTRY.get_policy(test_name).unwrap();

        let endpoints = create_n_endpoints_with_k_addresses(4, 1);
        send_resolver_update_to_policy(
            &mut child_manager,
            endpoints.clone(),
            builder,
            tcc.as_mut(),
        )
        .unwrap();
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
        let test_name = "stub-childmanager_aggregate_state_is_connecting_if_no_child_is_ready";
        let (mut rx_events, mut child_manager, mut tcc) =
            setup(create_verifying_funcs_for_aggregate_tests(), test_name);
        let builder: Arc<dyn LbPolicyBuilder> = GLOBAL_LB_REGISTRY.get_policy(test_name).unwrap();
        let endpoints = create_n_endpoints_with_k_addresses(3, 1);
        send_resolver_update_to_policy(
            &mut child_manager,
            endpoints.clone(),
            builder,
            tcc.as_mut(),
        )
        .unwrap();
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
        let test_name = "stub-childmanager_aggregate_state_is_idle_if_only_idle_and_failure";
        let (mut rx_events, mut child_manager, mut tcc) =
            setup(create_verifying_funcs_for_aggregate_tests(), test_name);
        let builder: Arc<dyn LbPolicyBuilder> = GLOBAL_LB_REGISTRY.get_policy(test_name).unwrap();

        let endpoints = create_n_endpoints_with_k_addresses(2, 1);
        send_resolver_update_to_policy(
            &mut child_manager,
            endpoints.clone(),
            builder,
            tcc.as_mut(),
        )
        .unwrap();
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
        let test_name =
            "stub-childmanager_aggregate_state_is_transient_failure_if_all_children_are";
        let (mut rx_events, mut child_manager, mut tcc) =
            setup(create_verifying_funcs_for_aggregate_tests(), test_name);
        let builder: Arc<dyn LbPolicyBuilder> = GLOBAL_LB_REGISTRY.get_policy(test_name).unwrap();
        let endpoints = create_n_endpoints_with_k_addresses(2, 1);
        send_resolver_update_to_policy(
            &mut child_manager,
            endpoints.clone(),
            builder,
            tcc.as_mut(),
        )
        .unwrap();
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

    struct ScheduleWorkStubData {
        requested_work: bool,
    }

    fn create_funcs_for_schedule_work_tests(name: &'static str) -> StubPolicyFuncs {
        StubPolicyFuncs {
            resolver_update: Some(Arc::new(move |data, _update, lbcfg, _controller| {
                if data.test_data.is_none() {
                    data.test_data = Some(Box::new(ScheduleWorkStubData {
                        requested_work: false,
                    }));
                }
                let stubdata = data
                    .test_data
                    .as_mut()
                    .unwrap()
                    .downcast_mut::<ScheduleWorkStubData>()
                    .unwrap();
                assert!(!stubdata.requested_work);
                if lbcfg
                    .unwrap()
                    .convert_to::<Mutex<HashMap<&'static str, ()>>>()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .contains_key(name)
                {
                    stubdata.requested_work = true;
                    data.lb_policy_options.work_scheduler.schedule_work();
                }
                Ok(())
            })),
            subchannel_update: None,
            work: Some(Arc::new(move |data, _controller| {
                println!("work called for {name}");
                let stubdata = data
                    .test_data
                    .as_mut()
                    .unwrap()
                    .downcast_mut::<ScheduleWorkStubData>()
                    .unwrap();
                stubdata.requested_work = false;
            })),
        }
    }

    // Tests that the child manager properly delegates to the children that
    // called schedule_work when work is called.
    #[tokio::test]
    async fn childmanager_schedule_work_works() {
        let name1 = "childmanager_schedule_work_works-one";
        let name2 = "childmanager_schedule_work_works-two";
        test_utils::reg_stub_policy(name1, create_funcs_for_schedule_work_tests(name1));
        test_utils::reg_stub_policy(name2, create_funcs_for_schedule_work_tests(name2));

        let (tx_events, mut rx_events) = mpsc::unbounded_channel::<TestEvent>();
        let mut tcc = TestChannelController {
            tx_events: tx_events.clone(),
        };

        let names = [name1, name2];
        let mut child_manager =
            ChildManager::new(default_runtime(), Arc::new(TestWorkScheduler { tx_events }));

        // Request that child one requests work.
        let cfg = LbConfig::new(Mutex::new(HashMap::<&'static str, ()>::new()));
        let children = cfg
            .convert_to::<Mutex<HashMap<&'static str, ()>>>()
            .unwrap();
        children.lock().unwrap().insert(name1, ());

        let updates = names.iter().map(|name| {
            let child_policy_builder: Arc<dyn LbPolicyBuilder> =
                GLOBAL_LB_REGISTRY.get_policy(name).unwrap();

            ChildUpdate {
                child_identifier: (),
                child_policy_builder,
                child_update: Some((ResolverUpdate::default(), Some(cfg.clone()))),
            }
        });
        child_manager.update(updates.clone(), &mut tcc).unwrap();

        // Confirm that child one has requested work.
        match rx_events.recv().await.unwrap() {
            TestEvent::ScheduleWork => {}
            other => panic!("unexpected event {:?}", other),
        };
        assert_eq!(child_manager.pending_work.lock().unwrap().len(), 1);
        let idx = *child_manager
            .pending_work
            .lock()
            .unwrap()
            .iter()
            .next()
            .unwrap();
        assert_eq!(child_manager.children[idx].builder.name(), name1);

        // Perform the work call and assert the pending_work set is empty.
        child_manager.work(&mut tcc);
        assert_eq!(child_manager.pending_work.lock().unwrap().len(), 0);

        // Now have both children request work.
        children.lock().unwrap().insert(name2, ());

        child_manager.update(updates.clone(), &mut tcc).unwrap();

        // Confirm that both children requested work.
        match rx_events.recv().await.unwrap() {
            TestEvent::ScheduleWork => {}
            other => panic!("unexpected event {:?}", other),
        };
        assert_eq!(child_manager.pending_work.lock().unwrap().len(), 2);

        // Perform the work call and assert the pending_work set is empty.
        child_manager.work(&mut tcc);
        assert_eq!(child_manager.pending_work.lock().unwrap().len(), 0);

        // Perform one final call to resolver_update which asserts that both
        // child policies had their work methods called.
        child_manager.update(updates, &mut tcc).unwrap();
    }
}
