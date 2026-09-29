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

use std::collections::HashMap;
use std::error::Error;
use std::fmt::Debug;
use std::hash::Hash;
use std::mem;
use std::sync::Arc;

use crate::client::ConnectivityState;
use crate::client::load_balancing::ChannelController;
use crate::client::load_balancing::DynLbPolicyBuilder;
use crate::client::load_balancing::LbPolicy;
use crate::client::load_balancing::LbPolicyBuilder;
use crate::client::load_balancing::LbPolicyOptions;
use crate::client::load_balancing::LbState;
use crate::client::load_balancing::Subchannel;
use crate::client::load_balancing::SubchannelState;
use crate::client::load_balancing::WorkData;
use crate::client::load_balancing::WorkScheduler;
use crate::client::name_resolution::ResolverUpdate;
use crate::core::Address;
use crate::rt::GrpcRuntime;

// An LbPolicy implementation that manages multiple children.
#[derive(Debug)]
pub struct ChildManager<T: Debug, B: LbPolicyBuilder = Arc<DynLbPolicyBuilder>> {
    handle_to_child_idx: HashMap<ChildHandle, usize>,
    children: Vec<Child<T, B>>,
    runtime: GrpcRuntime,
    updated: bool, // Set when any child updates its picker; cleared when accessed.
    work_scheduler: Arc<dyn WorkScheduler>,
}

#[non_exhaustive]
#[derive(Debug)]
pub struct Child<T, B: LbPolicyBuilder = Arc<DynLbPolicyBuilder>> {
    pub identifier: T,
    pub builder: B,
    pub state: LbState,
    policy: B::LbPolicy,
    work_scheduler: Arc<ChildWorkScheduler>,
}

/// A collection of data sent to a child of the ChildManager.
pub struct ChildUpdate<'a, T, B: LbPolicyBuilder = Arc<DynLbPolicyBuilder>> {
    /// The identifier the ChildManager should use for this child.
    pub child_identifier: T,
    /// The builder the ChildManager should use to create this child if it does
    /// not exist.  The child_policy_builder's name is effectively a part of the
    /// child_identifier.  If two identifiers are identical but have different
    /// builder names, they are treated as different children.
    pub child_policy_builder: B,
    /// The relevant ResolverUpdate and LbConfig to send to this child.  If
    /// None, then resolver_update will not be called on the child.  Should
    /// generally be Some for any new children, otherwise they will not be
    /// called.
    pub child_update: Option<(ResolverUpdate, &'a <B::LbPolicy as LbPolicy>::LbConfig)>,
}

impl<T, B> ChildManager<T, B>
where
    T: Debug + PartialEq + Hash + Eq + Send + Sync + 'static,
    B: LbPolicyBuilder,
{
    /// Creates a new ChildManager LB policy.  shard_update is called whenever a
    /// resolver_update operation occurs.
    pub fn new(runtime: GrpcRuntime, work_scheduler: Arc<dyn WorkScheduler>) -> Self {
        Self {
            handle_to_child_idx: Default::default(),
            children: Default::default(),
            runtime,
            work_scheduler,
            updated: false,
        }
    }

    /// Returns data for all current children.
    pub fn children(&self) -> impl Iterator<Item = &Child<T, B>> {
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

    /// Returns true if any child has updated its picker since the last call to
    /// child_updated.
    pub fn child_updated(&mut self) -> bool {
        mem::take(&mut self.updated)
    }

    /// Retains only the child policies specified by the iterator.
    ///
    /// If an ID is provided that does not exist in the ChildManager, it will be
    /// ignored.
    pub fn retain_children(&mut self, ids_builders: impl IntoIterator<Item = (T, B)>) {
        self.reset_children(ids_builders, true);
    }

    /// Resets the children and all state related to tracking them in accordance
    /// with the iterator provided.  When retain_only is true, any entry in
    /// ids_builders that is not in the current set of children will be ignored;
    /// otherwise a new child will be built for it.
    fn reset_children(
        &mut self,
        ids_builders: impl IntoIterator<Item = (T, B)>,
        retain_only: bool,
    ) {
        // Replace self.children with an empty vec.
        let old_children = mem::take(&mut self.children);

        // Build a map of the old children from their IDs for efficient lookups.
        // The builder is not retained: the one from ids_builders is used for any
        // child that is transferred.
        let mut old_children: HashMap<(&'static str, T), _> = old_children
            .into_iter()
            .map(|e| {
                (
                    (e.builder.name(), e.identifier),
                    (e.state, e.policy, e.work_scheduler),
                )
            })
            .collect();

        // Clear handle index map.
        self.handle_to_child_idx.clear();

        // Transfer children whose identifiers appear before and after the
        // update, and create new children.
        for (identifier, builder) in ids_builders {
            let k = (builder.name(), identifier);
            if let Some((state, policy, work_scheduler)) = old_children.remove(&k) {
                let new_child_idx = self.children.len();
                self.handle_to_child_idx
                    .insert(work_scheduler.handle.clone(), new_child_idx);
                self.children.push(Child {
                    builder,
                    identifier: k.1,
                    state,
                    policy,
                    work_scheduler,
                });
            } else if !retain_only {
                let handle = ChildHandle(Arc::new(()));
                let new_child_idx = self.children.len();
                self.handle_to_child_idx
                    .insert(handle.clone(), new_child_idx);
                let work_scheduler = Arc::new(ChildWorkScheduler {
                    work_scheduler: self.work_scheduler.clone(),
                    handle,
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
            }
        }
        // Anything left in old_children will just be Dropped and cleaned up.
    }

    /// Updates the ChildManager's children.
    ///
    /// `child_updates` is used to determine which children should exist (one
    /// for each item), how to construct them if they don't already, and what to
    /// send to their `resolver_update` methods, if anything.  Any existing
    /// children not present in child_updates will be removed.
    pub fn update<'a>(
        &mut self,
        child_updates: impl IntoIterator<Item = ChildUpdate<'a, T, B>>,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), String> {
        // Split the child updates into the IDs and builders, and the
        // ResolverUpdates/LbConfigs.
        let mut errs = vec![];
        let (ids_builders, updates): (Vec<_>, Vec<_>) = child_updates
            .into_iter()
            .map(|e| ((e.child_identifier, e.child_policy_builder), e.child_update))
            .unzip();

        self.reset_children(ids_builders, false);

        // Call resolver_update on all children.
        for (child, child_update) in self.children.iter_mut().zip(updates) {
            let Some((resolver_update, config)) = child_update else {
                continue;
            };
            let mut channel_controller =
                WrappedController::new(channel_controller, &mut child.state, &mut self.updated);
            if let Err(err) =
                child
                    .policy
                    .resolver_update(resolver_update, config, &mut channel_controller)
            {
                errs.push(err);
            }
        }
        if errs.is_empty() {
            Ok(())
        } else {
            let err = errs
                .into_iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            Err(err)
        }
    }

    /// Forwards the `resolver_update` and `config` to all current children.
    ///
    /// Returns the Result from calling into each child.
    pub fn resolver_update(
        &mut self,
        resolver_update: ResolverUpdate,
        config: &<B::LbPolicy as LbPolicy>::LbConfig,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let mut errs = Vec::with_capacity(self.children.len());
        for child in &mut self.children {
            let mut channel_controller =
                WrappedController::new(channel_controller, &mut child.state, &mut self.updated);
            if let Err(err) = child.policy.resolver_update(
                resolver_update.clone(),
                config,
                &mut channel_controller,
            ) {
                errs.push(err);
            }
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

    /// Calls work on the child that scheduled work via its work scheduler.
    pub fn work(&mut self, data: Option<WorkData>, channel_controller: &mut dyn ChannelController) {
        let Some(data) = data else {
            debug_assert!(false, "ChildManager::work called with None value");
            return;
        };
        let child_work_item = match data.downcast::<ChildWorkItem>() {
            Ok(item) => item,
            Err(data) => {
                debug_assert!(
                    false,
                    "ChildManager::work called with {data:?}; expected ChildWorkItem"
                );
                return;
            }
        };
        if let Some(&child_idx) = self.handle_to_child_idx.get(&child_work_item.handle) {
            let child = &mut self.children[child_idx];
            let mut channel_controller =
                WrappedController::new(channel_controller, &mut child.state, &mut self.updated);
            child
                .policy
                .work(child_work_item.data, &mut channel_controller);
        }
    }

    /// Calls exit_idle on all children.
    pub fn exit_idle(&mut self, channel_controller: &mut dyn ChannelController) {
        for child in &mut self.children {
            let mut channel_controller =
                WrappedController::new(channel_controller, &mut child.state, &mut self.updated);
            child.policy.exit_idle(&mut channel_controller);
        }
    }
}

struct WrappedController<'a> {
    channel_controller: &'a mut dyn ChannelController,
    child_state: &'a mut LbState,
    updated: &'a mut bool,
}

impl<'a> WrappedController<'a> {
    fn new(
        channel_controller: &'a mut dyn ChannelController,
        child_state: &'a mut LbState,
        updated: &'a mut bool,
    ) -> Self {
        Self {
            channel_controller,
            child_state,
            updated,
        }
    }
}

impl ChannelController for WrappedController<'_> {
    fn new_subchannel(
        &mut self,
        address: &Address,
        work_scheduler: Arc<dyn WorkScheduler>,
    ) -> (Arc<dyn Subchannel>, SubchannelState) {
        self.channel_controller
            .new_subchannel(address, work_scheduler)
    }

    fn update_picker(&mut self, update: LbState) {
        *self.child_state = update;
        *self.updated = true;
    }

    fn request_resolution(&mut self) {
        self.channel_controller.request_resolution();
    }
}

#[derive(Clone, Debug)]
struct ChildHandle(Arc<()>);

impl PartialEq for ChildHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for ChildHandle {}

impl std::hash::Hash for ChildHandle {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state);
    }
}

#[derive(Debug)]
struct ChildWorkItem {
    handle: ChildHandle,
    data: Option<WorkData>,
}

#[derive(Debug)]
struct ChildWorkScheduler {
    work_scheduler: Arc<dyn WorkScheduler>, // The real work scheduler of the channel.
    handle: ChildHandle,
}

impl WorkScheduler for ChildWorkScheduler {
    fn schedule_work(&self, data: Option<WorkData>) {
        let wrapped: Option<WorkData> = Some(Box::new(ChildWorkItem {
            handle: self.handle.clone(),
            data,
        }));
        self.work_scheduler.schedule_work(wrapped);
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashMap;
    use std::panic;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::mpsc;

    use crate::attributes::Attributes;
    use crate::client::ConnectivityState;
    use crate::client::load_balancing::DynLbConfig;
    use crate::client::load_balancing::DynLbPolicyBuilder;
    use crate::client::load_balancing::GLOBAL_LB_REGISTRY;
    use crate::client::load_balancing::LbState;
    use crate::client::load_balancing::QueuingPicker;
    use crate::client::load_balancing::Subchannel;
    use crate::client::load_balancing::SubchannelState;
    use crate::client::load_balancing::child_manager::ChildManager;
    use crate::client::load_balancing::child_manager::ChildUpdate;
    use crate::client::load_balancing::subchannel::SubchannelUpdate;
    use crate::client::load_balancing::test_utils;
    use crate::client::load_balancing::test_utils::StubPolicyFuncs;
    use crate::client::load_balancing::test_utils::TestChannelController;
    use crate::client::load_balancing::test_utils::TestEnv;
    use crate::client::load_balancing::test_utils::TestEvent;
    use crate::client::load_balancing::test_utils::TestWorkScheduler;
    use crate::client::name_resolution::Endpoint;
    use crate::client::name_resolution::ResolverUpdate;
    use crate::core::Address;
    use crate::rt::default_runtime;

    // Constructs the test environment for ChildManager tests, registering a
    // StubPolicy named `test_name` with the given funcs as the child policy.
    fn new_env(funcs: StubPolicyFuncs, test_name: &'static str) -> TestEnv<ChildManager<Endpoint>> {
        test_utils::reg_stub_policy(test_name, funcs);
        TestEnv::new(|work_scheduler| ChildManager::new(default_runtime(), work_scheduler))
    }

    impl TestEnv<ChildManager<Endpoint>> {
        // Sends a resolver update to the child manager with one child per
        // endpoint.
        fn send_resolver_update(
            &mut self,
            endpoints: Vec<Endpoint>,
            builder: Arc<DynLbPolicyBuilder>,
        ) -> Result<(), String> {
            let cfg = Arc::new(()) as DynLbConfig;
            let updates = endpoints.iter().map(|e| ChildUpdate {
                child_identifier: e.clone(),
                child_policy_builder: builder.clone(),
                child_update: Some((
                    ResolverUpdate {
                        attributes: Attributes::default(),
                        endpoints: Ok(vec![e.clone()]),
                        service_config: Ok(None),
                        resolution_note: None,
                    },
                    &cfg,
                )),
            });

            self.policy.update(updates, &mut self.tcc)
        }

        // Simulates a state change of `subchannel` and delivers the resulting
        // work item to the child manager.
        fn send_subchannel_update(
            &mut self,
            subchannel: &Arc<dyn Subchannel>,
            state: &SubchannelState,
        ) {
            self.expect_no_events();
            test_utils::schedule_subchannel_update(subchannel, state.clone());
            let data = self.expect_schedule_work();
            self.policy.work(data, &mut self.tcc);
        }

        // Verifies that the expected number of subchannels is created. Returns
        // the subchannels created.
        fn verify_subchannel_creation(
            &mut self,
            number_of_subchannels: usize,
        ) -> Vec<Arc<dyn Subchannel>> {
            let mut subchannels = Vec::new();
            for _ in 0..number_of_subchannels {
                subchannels.push(self.expect_new_subchannel());
            }
            subchannels
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
            });
        }
        endpoints
    }

    // Defines the functions resolver_update and work to test
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
                    controller.new_subchannel(
                        &endpoint.addresses[0],
                        data.lb_policy_options.work_scheduler.clone(),
                    );
                    Ok(())
                },
            )),
            // Closure for work. Sends a picker of the same state that was
            // passed to it in the subchannel update.
            work: Some(Arc::new(move |_data, data, controller| {
                let update = data
                    .expect("expected work data")
                    .downcast::<SubchannelUpdate>()
                    .expect("expected SubchannelUpdate");
                controller.update_picker(LbState {
                    connectivity_state: update.state.connectivity_state,
                    picker: Arc::new(QueuingPicker {}),
                });
            })),
            ..Default::default()
        }
    }

    // Tests the scenario where one child is READY and the rest are in
    // CONNECTING, IDLE, or TRANSIENT FAILURE. The child manager's
    // aggregate_states function should report READY.
    #[test]
    fn childmanager_aggregate_state_is_ready_if_any_child_is_ready() {
        let test_name = "stub-childmanager_aggregate_state_is_ready_if_any_child_is_ready";
        let mut env = new_env(create_verifying_funcs_for_aggregate_tests(), test_name);
        let builder: Arc<DynLbPolicyBuilder> = GLOBAL_LB_REGISTRY.get_policy(test_name).unwrap();

        let endpoints = create_n_endpoints_with_k_addresses(4, 1);
        env.send_resolver_update(endpoints.clone(), builder)
            .unwrap();
        let mut subchannels = vec![];
        for endpoint in endpoints {
            subchannels.push(
                env.verify_subchannel_creation(endpoint.addresses.len())
                    .remove(0),
            );
        }

        let mut subchannels = subchannels.into_iter();
        env.send_subchannel_update(
            &subchannels.next().unwrap(),
            &SubchannelState::transient_failure("n/a"),
        );
        env.send_subchannel_update(&subchannels.next().unwrap(), &SubchannelState::idle());
        env.send_subchannel_update(&subchannels.next().unwrap(), &SubchannelState::connecting());
        env.send_subchannel_update(&subchannels.next().unwrap(), &SubchannelState::ready());
        assert_eq!(env.policy.aggregate_states(), ConnectivityState::Ready);
    }

    // Tests the scenario where no children are READY and the children are in
    // CONNECTING, IDLE, or TRANSIENT FAILURE. The child manager's
    // aggregate_states function should report CONNECTING.
    #[test]
    fn childmanager_aggregate_state_is_connecting_if_no_child_is_ready() {
        let test_name = "stub-childmanager_aggregate_state_is_connecting_if_no_child_is_ready";
        let mut env = new_env(create_verifying_funcs_for_aggregate_tests(), test_name);
        let builder: Arc<DynLbPolicyBuilder> = GLOBAL_LB_REGISTRY.get_policy(test_name).unwrap();
        let endpoints = create_n_endpoints_with_k_addresses(3, 1);
        env.send_resolver_update(endpoints.clone(), builder)
            .unwrap();
        let mut subchannels = vec![];
        for endpoint in endpoints {
            subchannels.push(
                env.verify_subchannel_creation(endpoint.addresses.len())
                    .remove(0),
            );
        }
        let mut subchannels = subchannels.into_iter();
        env.send_subchannel_update(
            &subchannels.next().unwrap(),
            &SubchannelState::transient_failure("n/a"),
        );
        env.send_subchannel_update(&subchannels.next().unwrap(), &SubchannelState::idle());
        env.send_subchannel_update(&subchannels.next().unwrap(), &SubchannelState::connecting());

        assert_eq!(env.policy.aggregate_states(), ConnectivityState::Connecting);
    }

    // Tests the scenario where no children are READY or CONNECTING and the
    // children are in IDLE, or TRANSIENT FAILURE. The child manager's
    // aggregate_states function should report IDLE.
    #[test]
    fn childmanager_aggregate_state_is_idle_if_only_idle_and_failure() {
        let test_name = "stub-childmanager_aggregate_state_is_idle_if_only_idle_and_failure";
        let mut env = new_env(create_verifying_funcs_for_aggregate_tests(), test_name);
        let builder: Arc<DynLbPolicyBuilder> = GLOBAL_LB_REGISTRY.get_policy(test_name).unwrap();

        let endpoints = create_n_endpoints_with_k_addresses(2, 1);
        env.send_resolver_update(endpoints.clone(), builder)
            .unwrap();
        let mut subchannels = vec![];
        for endpoint in endpoints {
            subchannels.push(
                env.verify_subchannel_creation(endpoint.addresses.len())
                    .remove(0),
            );
        }
        let mut subchannels = subchannels.into_iter();
        env.send_subchannel_update(
            &subchannels.next().unwrap(),
            &SubchannelState::transient_failure("n/a"),
        );
        env.send_subchannel_update(&subchannels.next().unwrap(), &SubchannelState::idle());
        assert_eq!(env.policy.aggregate_states(), ConnectivityState::Idle);
    }

    // Tests the scenario where no children are READY, CONNECTING, or IDLE and
    // all children are in TRANSIENT FAILURE. The child manager's
    // aggregate_states function should report TRANSIENT FAILURE.
    #[test]
    fn childmanager_aggregate_state_is_transient_failure_if_all_children_are() {
        let test_name =
            "stub-childmanager_aggregate_state_is_transient_failure_if_all_children_are";
        let mut env = new_env(create_verifying_funcs_for_aggregate_tests(), test_name);
        let builder: Arc<DynLbPolicyBuilder> = GLOBAL_LB_REGISTRY.get_policy(test_name).unwrap();
        let endpoints = create_n_endpoints_with_k_addresses(2, 1);
        env.send_resolver_update(endpoints.clone(), builder)
            .unwrap();
        let mut subchannels = vec![];
        for endpoint in endpoints {
            subchannels.push(
                env.verify_subchannel_creation(endpoint.addresses.len())
                    .remove(0),
            );
        }
        let mut subchannels = subchannels.into_iter();
        env.send_subchannel_update(
            &subchannels.next().unwrap(),
            &SubchannelState::transient_failure("n/a"),
        );
        env.send_subchannel_update(
            &subchannels.next().unwrap(),
            &SubchannelState::transient_failure("n/a"),
        );
        assert_eq!(
            env.policy.aggregate_states(),
            ConnectivityState::TransientFailure
        );
    }

    struct ScheduleWorkStubData {
        requested_work: bool,
    }

    fn create_funcs_for_schedule_work_tests(
        name: &'static str,
        work_called: Arc<Mutex<HashMap<&'static str, bool>>>,
    ) -> StubPolicyFuncs {
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
                    .downcast_ref::<Mutex<HashMap<&'static str, ()>>>()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .contains_key(name)
                {
                    stubdata.requested_work = true;
                    data.lb_policy_options.work_scheduler.schedule_work(None);
                }
                Ok(())
            })),
            work: Some(Arc::new(move |data, _workitem, _controller| {
                println!("work called for {name}");
                let stubdata = data
                    .test_data
                    .as_mut()
                    .unwrap()
                    .downcast_mut::<ScheduleWorkStubData>()
                    .unwrap();
                stubdata.requested_work = false;
                work_called.lock().unwrap().insert(name, true);
            })),
            ..Default::default()
        }
    }

    // Tests that the child manager properly delegates to the children that
    // called schedule_work when work is called.
    #[test]
    fn childmanager_schedule_work_works() {
        let name1 = "childmanager_schedule_work_works-one";
        let name2 = "childmanager_schedule_work_works-two";
        let work_called = Arc::new(Mutex::new(HashMap::<&'static str, bool>::new()));

        test_utils::reg_stub_policy(
            name1,
            create_funcs_for_schedule_work_tests(name1, work_called.clone()),
        );
        test_utils::reg_stub_policy(
            name2,
            create_funcs_for_schedule_work_tests(name2, work_called.clone()),
        );

        let (tx_events, _rx_events) = mpsc::channel::<TestEvent>();
        let (tx_work, rx_work) = mpsc::channel();
        let mut tcc = TestChannelController { tx_events };

        let names = [name1, name2];
        let mut child_manager =
            ChildManager::new(default_runtime(), Arc::new(TestWorkScheduler { tx_work }));

        // Request that child one requests work.
        let cfg = Arc::new(Mutex::new(HashMap::<&'static str, ()>::new())) as DynLbConfig;
        let children = cfg
            .downcast_ref::<Mutex<HashMap<&'static str, ()>>>()
            .unwrap();
        children.lock().unwrap().insert(name1, ());

        let updates = names.iter().map(|name| {
            let child_policy_builder: Arc<DynLbPolicyBuilder> =
                GLOBAL_LB_REGISTRY.get_policy(name).unwrap();

            ChildUpdate {
                child_identifier: (),
                child_policy_builder,
                child_update: Some((ResolverUpdate::default(), &cfg)),
            }
        });
        child_manager.update(updates.clone(), &mut tcc).unwrap();

        let child1_handle = child_manager.children[0].work_scheduler.handle.clone();
        let child2_handle = child_manager.children[1].work_scheduler.handle.clone();

        // Confirm that child one has requested work.
        let data = rx_work.recv().unwrap();
        // Validate data indicates the child to call.
        {
            let wrapped = data
                .as_ref()
                .unwrap()
                .downcast_ref::<super::ChildWorkItem>()
                .unwrap();
            assert_eq!(wrapped.handle, child1_handle);
        }

        // Perform the work call.
        child_manager.work(data, &mut tcc);
        // Validate that this call made it to the child.
        assert!(*work_called.lock().unwrap().get(name1).unwrap_or(&false));
        assert!(!*work_called.lock().unwrap().get(name2).unwrap_or(&false));

        // Clear work_called state.
        work_called.lock().unwrap().clear();

        // Now request that both children request work.
        children.lock().unwrap().insert(name2, ());

        child_manager.update(updates.clone(), &mut tcc).unwrap();

        // Expect two work items. Since they both happened, let's collect them.
        let mut works = vec![];
        for _ in 0..2 {
            works.push(rx_work.recv().unwrap());
        }

        // We expect one work item for child1 and one for child2.
        let mut child1_work = None;
        let mut child2_work = None;

        for work in works {
            let handle = work
                .as_ref()
                .unwrap()
                .downcast_ref::<super::ChildWorkItem>()
                .unwrap()
                .handle
                .clone();
            if handle == child1_handle {
                child1_work = Some(work);
            } else if handle == child2_handle {
                child2_work = Some(work);
            } else {
                panic!("unexpected child handle");
            }
        }

        let child1_work = child1_work.expect("should have scheduled work for child 1");
        let child2_work = child2_work.expect("should have scheduled work for child 2");

        // Call work for child 1.
        child_manager.work(child1_work, &mut tcc);
        assert!(*work_called.lock().unwrap().get(name1).unwrap_or(&false));
        assert!(!*work_called.lock().unwrap().get(name2).unwrap_or(&false));

        // Call work for child 2.
        child_manager.work(child2_work, &mut tcc);
        assert!(*work_called.lock().unwrap().get(name2).unwrap_or(&false));
    }
}
