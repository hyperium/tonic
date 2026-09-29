/*
 *
 * Copyright 2026 gRPC authors.
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

use std::collections::HashMap;
use std::hash::Hash;
use std::hash::Hasher;
use std::sync::Arc;

use crate::StatusCodeError;
use crate::StatusError;
use crate::client::RequestHeaders;
use crate::client::load_balancing::ChannelController;
use crate::client::load_balancing::LbPolicy;
use crate::client::load_balancing::LbState;
use crate::client::load_balancing::PickResult;
use crate::client::load_balancing::Picker;
use crate::client::load_balancing::WorkData;
use crate::client::load_balancing::WorkScheduler;
use crate::client::load_balancing::subchannel::ForwardingSubchannel;
use crate::client::load_balancing::subchannel::Subchannel;
use crate::client::load_balancing::subchannel::SubchannelState;
use crate::client::load_balancing::subchannel::SubchannelUpdate;
use crate::client::load_balancing::subchannel::WeakSubchannel;
use crate::client::name_resolution::ResolverUpdate;
use crate::core::Address;

/// Implements subchannel sharing for T.  Whenever T creates a subchannel, this
/// policy wraps what the channel returns, and if another subchannel is created
/// for the same address, the first subchannel will be reused to back the second
/// subchannel.
#[derive(Debug)]
pub(crate) struct SubchannelSharing<T> {
    delegate: T,
    /// Routes work back into this policy's `work` method.  Everything that
    /// needs to touch `inner` from another thread goes through here instead.
    work_scheduler: Arc<dyn WorkScheduler>,
    inner: Inner,
}

impl<T> SubchannelSharing<T> {
    pub(crate) fn new(delegate: T, work_scheduler: Arc<dyn WorkScheduler>) -> Self {
        Self {
            delegate,
            work_scheduler,
            inner: Inner {
                subchannels_by_address: HashMap::new(),
                subchannels_int_to_ext: HashMap::new(),
            },
        }
    }

    /// Translates a state change of an internal subchannel into one update per
    /// external subchannel backed by it, delivered via the WorkScheduler that
    /// the creating policy passed to `new_subchannel`.
    fn deliver_subchannel_update(&mut self, update: SubchannelUpdate) {
        let Some(entry) = self
            .inner
            .subchannels_int_to_ext
            .get_mut(&update.subchannel)
        else {
            // The internal subchannel has been released.  Nobody is interested
            // in this update.
            return;
        };
        // Remember the state for any external subchannels created later.
        entry.state = update.state.clone();
        for (weak, work_scheduler) in entry.ext_subchannels.iter() {
            let Some(ext_subchannel) = weak.upgrade() else {
                continue;
            };
            work_scheduler.schedule_work(Some(Box::new(SubchannelUpdate::new(
                ext_subchannel,
                update.state.clone(),
            ))));
        }
    }

    /// Forgets any external subchannels backed by `int_subchannel` that have
    /// been dropped, and the internal subchannel itself once the last one goes
    /// away.
    fn release_subchannel(&mut self, int_subchannel: &Arc<dyn Subchannel>) {
        let Some(entry) = self.inner.subchannels_int_to_ext.get_mut(int_subchannel) else {
            return;
        };
        // Note that since we iterate over every weak subchannel, performance is
        // predicated on not extensively sharing subchannels.  If subchannels
        // are commonly shared many times, we could instead store an
        // Option<WeakSubchannel> for ourselves inside SharedSubchannel to allow
        // us to do something like `ext_subchannels.remove(&self.weak_self)`
        // (which would be constructed using Arc::new_cyclic).
        entry
            .ext_subchannels
            .retain(|weak, _| weak.strong_count() != 0);
        if entry.ext_subchannels.is_empty() {
            // This was the last external subchannel using this internal
            // subchannel.  Drop the internal subchannel.
            self.inner.subchannels_int_to_ext.remove(int_subchannel);
            self.inner
                .subchannels_by_address
                .remove(&int_subchannel.address());
        }
    }
}

#[derive(Debug)]
struct Inner {
    subchannels_by_address: HashMap<Address, Arc<dyn Subchannel>>,
    subchannels_int_to_ext: HashMap<Arc<dyn Subchannel>, InternalSubchannelEntry>,
}

/// Tracks everything known about one internal (shared) subchannel.
#[derive(Debug)]
struct InternalSubchannelEntry {
    /// The most recent state of the internal subchannel.  Returned to policies
    /// that create another external subchannel for the same address.
    state: SubchannelState,
    /// The external subchannels backed by this internal subchannel, along with
    /// the WorkScheduler used to deliver updates about each of them.
    ext_subchannels: HashMap<WeakSubchannel, Arc<dyn WorkScheduler>>,
}

#[derive(Debug)]
enum SharingWork {
    /// A state change of an internal subchannel, from SubchannelUpdateForwarder.
    InternalUpdate(SubchannelUpdate),
    /// An external subchannel was dropped, from SharedSubchannel::drop.
    Released(Arc<dyn Subchannel>),
}

impl<T: LbPolicy> LbPolicy for SubchannelSharing<T> {
    type LbConfig = T::LbConfig;

    fn resolver_update(
        &mut self,
        update: ResolverUpdate,
        config: &T::LbConfig,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), String> {
        let mut channel_controller = SharingChannelController {
            inner: &mut self.inner,
            work_scheduler: &self.work_scheduler,
            delegate: channel_controller,
        };
        self.delegate
            .resolver_update(update, config, &mut channel_controller)
    }

    fn work(&mut self, data: Option<WorkData>, channel_controller: &mut dyn ChannelController) {
        // Handle the work this policy scheduled for itself.  Both kinds
        // originate on arbitrary threads and are handled here so that `inner`
        // is only ever touched from the channel's work loop.
        let data = match data.map(|d| d.downcast::<SharingWork>()) {
            Some(Ok(work)) => {
                return match *work {
                    SharingWork::InternalUpdate(u) => self.deliver_subchannel_update(u),
                    SharingWork::Released(sc) => self.release_subchannel(&sc),
                };
            }
            Some(Err(data)) => Some(data),
            None => None,
        };

        // Everything else is intended for the delegate.
        let mut channel_controller = SharingChannelController {
            inner: &mut self.inner,
            work_scheduler: &self.work_scheduler,
            delegate: channel_controller,
        };
        self.delegate.work(data, &mut channel_controller);
    }

    fn exit_idle(&mut self, channel_controller: &mut dyn ChannelController) {
        let mut channel_controller = SharingChannelController {
            inner: &mut self.inner,
            work_scheduler: &self.work_scheduler,
            delegate: channel_controller,
        };
        self.delegate.exit_idle(&mut channel_controller);
    }
}

#[derive(Debug)]
struct SharedSubchannel {
    delegate: Arc<dyn Subchannel>,
    /// Routes work back into the SubchannelSharing policy, used to report that
    /// this subchannel has been dropped.
    work_scheduler: Arc<dyn WorkScheduler>,
}

impl PartialEq for SharedSubchannel {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::addr_eq(self, other)
    }
}

impl Eq for SharedSubchannel {}

impl Hash for SharedSubchannel {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self as *const Self).hash(state);
    }
}

impl ForwardingSubchannel for SharedSubchannel {
    fn delegate(&self) -> &Arc<dyn Subchannel> {
        &self.delegate
    }
}

impl Drop for SharedSubchannel {
    fn drop(&mut self) {
        // This can run on any thread, since pickers hold external subchannels,
        // so the maps must not be touched here.  Ask the policy to clean up
        // instead.  The cloned delegate identifies the entry to clean up, and
        // keeps the internal subchannel alive until that happens.
        self.work_scheduler
            .schedule_work(Some(Box::new(SharingWork::Released(self.delegate.clone()))));
    }
}

/// The WorkScheduler given to the channel for every internal subchannel.
///
/// The channel may call this from any thread, and even before `new_subchannel`
/// has returned, i.e. before the subchannel has been registered.  So it does no
/// work itself: it just asks for the update to be redelivered to the
/// SubchannelSharing policy's `work` method, where the registration is
/// guaranteed to be complete and the maps can be accessed without locking.
#[derive(Debug)]
struct SubchannelUpdateForwarder {
    work_scheduler: Arc<dyn WorkScheduler>,
}

impl WorkScheduler for SubchannelUpdateForwarder {
    fn schedule_work(&self, data: Option<WorkData>) {
        let update = match data.map(|data| data.downcast::<SubchannelUpdate>()) {
            Some(Ok(update)) => update,
            other => {
                debug_assert!(
                    false,
                    "subchannel scheduled work with {other:?}; expected a SubchannelUpdate"
                );
                return;
            }
        };
        self.work_scheduler
            .schedule_work(Some(Box::new(SharingWork::InternalUpdate(*update))));
    }
}

struct SharingChannelController<'a> {
    inner: &'a mut Inner,
    /// Routes work back into the SubchannelSharing policy.  Handed to every
    /// subchannel this controller creates.
    work_scheduler: &'a Arc<dyn WorkScheduler>,
    delegate: &'a mut dyn ChannelController,
}

impl ChannelController for SharingChannelController<'_> {
    fn new_subchannel(
        &mut self,
        address: &Address,
        work_scheduler: Arc<dyn WorkScheduler>,
    ) -> (Arc<dyn Subchannel>, SubchannelState) {
        // Find the existing internal subchannel with this address, if any.
        let existing = self.inner.subchannels_by_address.get(address).cloned();

        let (int_subchannel, new_state) = match existing {
            Some(int_subchannel) => (int_subchannel, None),
            None => {
                // Create a new internal subchannel.  The channel may report a
                // state for it before this call even returns, but that update
                // is only queued by the SubchannelUpdateForwarder; it is not
                // processed until `work` is called, by which time the
                // registration below has completed.
                let (int_subchannel, state) = self.delegate.new_subchannel(
                    address,
                    Arc::new(SubchannelUpdateForwarder {
                        work_scheduler: self.work_scheduler.clone(),
                    }),
                );
                self.inner
                    .subchannels_by_address
                    .insert(address.clone(), int_subchannel.clone());
                (int_subchannel, Some(state))
            }
        };

        let ext_subchannel: Arc<dyn Subchannel> = Arc::new(SharedSubchannel {
            delegate: int_subchannel.clone(),
            work_scheduler: self.work_scheduler.clone(),
        });

        // Insert a weak reference to this new external subchannel, along with
        // the work scheduler used to deliver its updates, into the int->ext
        // map.
        let entry = self
            .inner
            .subchannels_int_to_ext
            .entry(int_subchannel)
            .or_insert_with(|| InternalSubchannelEntry {
                state: new_state.expect("new internal subchannel must have a state"),
                ext_subchannels: HashMap::new(),
            });

        entry
            .ext_subchannels
            .insert((&ext_subchannel).into(), work_scheduler);

        let state = entry.state.clone();
        (ext_subchannel, state)
    }

    fn update_picker(&mut self, mut update: LbState) {
        update.picker = UnwrapPicker::new_arc(update.picker);
        self.delegate.update_picker(update);
    }

    fn request_resolution(&mut self) {
        self.delegate.request_resolution();
    }
}

#[derive(Debug)]
struct UnwrapPicker {
    delegate: Arc<dyn Picker>,
}

impl UnwrapPicker {
    fn new_arc(delegate: Arc<dyn Picker>) -> Arc<Self> {
        Arc::new(Self { delegate })
    }
}

impl Picker for UnwrapPicker {
    fn pick(&self, request: &RequestHeaders) -> PickResult {
        let result = self.delegate.pick(request);
        match result {
            PickResult::Pick(mut pick) => {
                let Some(subchannel) = pick.subchannel.downcast_ref::<SharedSubchannel>() else {
                    return PickResult::Fail(StatusError::new(
                        StatusCodeError::Internal,
                        format!(
                            "received unexpected subchannel type: {:?}",
                            pick.subchannel.type_id()
                        ),
                    ));
                };
                pick.subchannel = subchannel.delegate.clone();
                PickResult::Pick(pick)
            }
            _ => result,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::mpsc;

    use super::*;
    use crate::client::ConnectivityState;
    use crate::client::load_balancing::DynLbConfig;
    use crate::client::load_balancing::LbPolicy;
    use crate::client::load_balancing::LbPolicyOptions;
    use crate::client::load_balancing::Pick;
    use crate::client::load_balancing::PickResult;
    use crate::client::load_balancing::Picker;
    use crate::client::load_balancing::subchannel::SubchannelState;
    use crate::client::load_balancing::test_utils::StubPolicy;
    use crate::client::load_balancing::test_utils::StubPolicyFuncs;
    use crate::client::load_balancing::test_utils::TestEnv;
    use crate::client::load_balancing::test_utils::TestEvent;
    use crate::client::load_balancing::test_utils::TestSubchannel;
    use crate::client::load_balancing::test_utils::TestWorkScheduler;
    use crate::client::load_balancing::test_utils::new_request_headers;
    use crate::client::load_balancing::test_utils::run_pending_work;
    use crate::client::name_resolution::ResolverUpdate;
    use crate::core::Address;
    use crate::metadata::MetadataMap;
    use crate::rt::default_runtime;

    // Constructs the test environment for SubchannelSharing tests, wrapping a
    // StubPolicy with the given funcs.
    //
    // Subchannel updates take two trips through the work scheduler: one to
    // SubchannelSharing and one to the delegate.  Tests must call run_work
    // after send_subchannel_update to deliver the second.
    fn new_env(funcs: StubPolicyFuncs) -> TestEnv<SubchannelSharing<StubPolicy>> {
        TestEnv::new(|work_scheduler| {
            let options = LbPolicyOptions {
                work_scheduler: work_scheduler.clone(),
                runtime: default_runtime(),
            };
            SubchannelSharing::new(StubPolicy::new(funcs, options), work_scheduler)
        })
    }

    // Tests that a single subchannel creation is properly forwarded to the
    // underlying channel controller and the created shared subchannel seen by
    // the delegate policy contains the real one.
    #[test]
    fn test_single_subchannel() {
        let sc_out = Arc::new(Mutex::new(None));
        let sc_out_clone = sc_out.clone();

        let mut env = new_env(StubPolicyFuncs {
            work: Some(Arc::new(move |data, _workitem, cc| {
                let addr = Address {
                    address: "127.0.0.1:80".to_string().into(),
                    ..Default::default()
                };
                let sc = cc
                    .new_subchannel(&addr, data.lb_policy_options.work_scheduler.clone())
                    .0;
                *sc_out_clone.lock().unwrap() = Some(sc);
            })),
            ..Default::default()
        });

        env.policy.work(None, &mut env.tcc);
        let internal_sc = env.expect_new_subchannel();

        let external_sc = sc_out.lock().unwrap().take().unwrap();
        let shared = external_sc.downcast_ref::<SharedSubchannel>().unwrap();
        assert!(Arc::ptr_eq(&shared.delegate, &internal_sc));
    }

    // Tests that when a delegate policy creates multiple subchannels with the
    // same address, they share the same delegate subchannel from the underlying
    // channel controller.
    #[test]
    fn test_multiple_subchannels_same_address() {
        let sc_out1 = Arc::new(Mutex::new(None));
        let sc_out1_clone = sc_out1.clone();
        let sc_out2 = Arc::new(Mutex::new(None));
        let sc_out2_clone = sc_out2.clone();

        let mut env = new_env(StubPolicyFuncs {
            work: Some(Arc::new(move |data, _workitem, cc| {
                let addr = Address {
                    address: "127.0.0.1:80".to_string().into(),
                    ..Default::default()
                };
                *sc_out1_clone.lock().unwrap() = Some(
                    cc.new_subchannel(&addr, data.lb_policy_options.work_scheduler.clone())
                        .0,
                );
                *sc_out2_clone.lock().unwrap() = Some(
                    cc.new_subchannel(&addr, data.lb_policy_options.work_scheduler.clone())
                        .0,
                );
            })),
            ..Default::default()
        });

        env.policy.work(None, &mut env.tcc);

        // Confirm that only one new_subchannel was seen by the underlying
        // channel controller.
        let internal_sc = env.expect_new_subchannel();
        env.expect_no_events();

        // Confirm that both SharedSubchannels seen by the delegate are unique
        // but share the same underlying subchannel.
        let external_sc1 = sc_out1.lock().unwrap().take().unwrap();
        let external_sc2 = sc_out2.lock().unwrap().take().unwrap();

        let shared1 = external_sc1.downcast_ref::<SharedSubchannel>().unwrap();
        let shared2 = external_sc2.downcast_ref::<SharedSubchannel>().unwrap();

        assert!(Arc::ptr_eq(&shared1.delegate, &internal_sc));
        assert!(Arc::ptr_eq(&shared2.delegate, &internal_sc));
        assert!(!Arc::ptr_eq(&external_sc1, &external_sc2));
        assert_ne!(&external_sc1, &external_sc2);
    }

    // Tests that when the delegate creates subchannels with different
    // addresses, they get different internal subchannels.
    #[test]
    fn test_multiple_subchannels_different_addresses() {
        let sc_out1 = Arc::new(Mutex::new(None));
        let sc_out1_clone = sc_out1.clone();
        let sc_out2 = Arc::new(Mutex::new(None));
        let sc_out2_clone = sc_out2.clone();

        let mut env = new_env(StubPolicyFuncs {
            work: Some(Arc::new(move |data, _workitem, cc| {
                let addr1 = Address {
                    address: "127.0.0.1:80".to_string().into(),
                    ..Default::default()
                };
                let addr2 = Address {
                    address: "127.0.0.2:80".to_string().into(),
                    ..Default::default()
                };
                *sc_out1_clone.lock().unwrap() = Some(
                    cc.new_subchannel(&addr1, data.lb_policy_options.work_scheduler.clone())
                        .0,
                );
                *sc_out2_clone.lock().unwrap() = Some(
                    cc.new_subchannel(&addr2, data.lb_policy_options.work_scheduler.clone())
                        .0,
                );
            })),
            ..Default::default()
        });

        env.policy.work(None, &mut env.tcc);

        // Verify that two new_subchannel calls occurred.
        env.expect_new_subchannel();
        env.expect_new_subchannel();
        env.expect_no_events();

        // Verify that the two subchannels contain different delegates.
        let external_sc1 = sc_out1.lock().unwrap().take().unwrap();
        let external_sc2 = sc_out2.lock().unwrap().take().unwrap();

        let shared1 = external_sc1.downcast_ref::<SharedSubchannel>().unwrap();
        let shared2 = external_sc2.downcast_ref::<SharedSubchannel>().unwrap();

        assert!(!Arc::ptr_eq(&shared1.delegate, &shared2.delegate));
    }

    // Tests that when subchannels are dropped, they are removed from the
    // sharing map.
    #[test]
    fn test_subchannel_cleanup_on_drop() {
        let update_calls = Arc::new(Mutex::new(0));
        let update_calls_clone = update_calls.clone();

        let sc_out1 = Arc::new(Mutex::new(None));
        let sc_out1_clone = sc_out1.clone();
        let sc_out2 = Arc::new(Mutex::new(None));
        let sc_out2_clone = sc_out2.clone();
        let sc_out3 = Arc::new(Mutex::new(None));
        let sc_out3_clone = sc_out3.clone();

        let work_calls = Arc::new(Mutex::new(0));
        let work_calls_clone = work_calls.clone();

        let mut env = new_env(StubPolicyFuncs {
            work: Some(Arc::new(move |data, work_item, cc| {
                if let Some(Ok(_update)) = work_item.map(|d| d.downcast::<SubchannelUpdate>()) {
                    *update_calls_clone.lock().unwrap() += 1;
                    return;
                }
                let addr = Address {
                    address: "127.0.0.1:80".to_string().into(),
                    ..Default::default()
                };
                let mut num_calls = work_calls_clone.lock().unwrap();
                if *num_calls == 0 {
                    *sc_out1_clone.lock().unwrap() = Some(
                        cc.new_subchannel(&addr, data.lb_policy_options.work_scheduler.clone())
                            .0,
                    );
                    *sc_out2_clone.lock().unwrap() = Some(
                        cc.new_subchannel(&addr, data.lb_policy_options.work_scheduler.clone())
                            .0,
                    );
                } else if *num_calls == 1 {
                    *sc_out3_clone.lock().unwrap() = Some(
                        cc.new_subchannel(&addr, data.lb_policy_options.work_scheduler.clone())
                            .0,
                    );
                }
                *num_calls += 1;
            })),
            ..Default::default()
        });

        // The first call to work should create sc1 and sc2.
        env.policy.work(None, &mut env.tcc);
        env.expect_new_subchannel();

        let external_sc1 = sc_out1.lock().unwrap().take().unwrap();
        let external_sc2 = sc_out2.lock().unwrap().take().unwrap();

        let internal_sc = external_sc1
            .downcast_ref::<SharedSubchannel>()
            .unwrap()
            .delegate
            .clone();
        let state = SubchannelState::idle();

        // Perform a subchannel update and confirm that two calls are made to
        // the delegate.
        env.send_subchannel_update(&internal_sc, &state);
        env.run_work();
        assert_eq!(*update_calls.lock().unwrap(), 2);

        // Drop one external subchannel and process the work it produces.
        drop(external_sc1);
        env.run_work();

        // Perform a subchannel update and confirm that only one call is made.
        *update_calls.lock().unwrap() = 0;
        env.send_subchannel_update(&internal_sc, &state);
        env.run_work();
        assert_eq!(*update_calls.lock().unwrap(), 1);

        // We should have 4 strong references to the internal subchannel: ours,
        // external_sc2, and the maps.
        assert_eq!(Arc::strong_count(&internal_sc), 4);

        // Drop the other subchannel.  Cleanup happens when the policy next
        // does work, so until then the maps still reference the internal
        // subchannel, as does the pending cleanup work item.
        drop(external_sc2);
        env.run_work();

        // Now there should be only our reference left to the internal
        // subchannel: ours.
        assert_eq!(Arc::strong_count(&internal_sc), 1);

        // Perform a subchannel update and confirm zero calls are made.
        *update_calls.lock().unwrap() = 0;
        env.send_subchannel_update(&internal_sc, &state);
        env.expect_no_events();
        assert_eq!(*update_calls.lock().unwrap(), 0);

        // Create a subchannel with the same address again and confirm that a
        // new underlying subchannel is created.
        env.policy.work(None, &mut env.tcc);
        env.expect_new_subchannel();

        let external_sc3 = sc_out3.lock().unwrap().take().unwrap();
        let shared_sc3 = external_sc3.downcast_ref::<SharedSubchannel>().unwrap();

        // Confirm a new subchannel was created.
        assert!(!Arc::ptr_eq(&shared_sc3.delegate, &internal_sc));
    }

    // Tests that single subchannel updates are sent to the delegate for every
    // duplicated shared subchannel.
    #[test]
    fn test_subchannel_update_broadcasts() {
        let update_calls = Arc::new(Mutex::new(0));
        let update_calls_clone = update_calls.clone();

        let sc_out1 = Arc::new(Mutex::new(None));
        let sc_out1_clone = sc_out1.clone();
        let sc_out2 = Arc::new(Mutex::new(None));
        let sc_out2_clone = sc_out2.clone();

        let mut env = new_env(StubPolicyFuncs {
            work: Some(Arc::new(move |data, work_item, cc| {
                if let Some(Ok(_update)) = work_item.map(|d| d.downcast::<SubchannelUpdate>()) {
                    *update_calls_clone.lock().unwrap() += 1;
                    return;
                }
                let addr = Address {
                    address: "127.0.0.1:80".to_string().into(),
                    ..Default::default()
                };
                *sc_out1_clone.lock().unwrap() = Some(
                    cc.new_subchannel(&addr, data.lb_policy_options.work_scheduler.clone())
                        .0,
                );
                *sc_out2_clone.lock().unwrap() = Some(
                    cc.new_subchannel(&addr, data.lb_policy_options.work_scheduler.clone())
                        .0,
                );
            })),
            ..Default::default()
        });

        env.policy.work(None, &mut env.tcc);
        env.expect_new_subchannel();

        let external_sc1 = sc_out1.lock().unwrap().take().unwrap();
        let external_sc2 = sc_out2.lock().unwrap().take().unwrap();

        let internal_sc = external_sc1
            .downcast_ref::<SharedSubchannel>()
            .unwrap()
            .delegate
            .clone();
        let state = SubchannelState::idle();

        // Verify that two delegated update calls are made.
        env.send_subchannel_update(&internal_sc, &state);
        env.run_work();
        assert_eq!(*update_calls.lock().unwrap(), 2);

        // Drop one and verify that one delegated update call is made.
        drop(external_sc1);
        env.run_work();
        env.send_subchannel_update(&internal_sc, &state);
        env.run_work();
        assert_eq!(*update_calls.lock().unwrap(), 3);
    }

    // Tests that the picker properly unwraps the shared subchannel into the
    // underlying subchannel.
    #[test]
    fn test_picker_unwraps_shared_subchannel() {
        let sc_out = Arc::new(Mutex::new(None));
        let sc_out_clone = sc_out.clone();

        let mut env = new_env(StubPolicyFuncs {
            work: Some(Arc::new(move |data, _workitem, cc| {
                let addr = Address {
                    address: "127.0.0.1:80".to_string().into(),
                    ..Default::default()
                };
                let sc = cc
                    .new_subchannel(&addr, data.lb_policy_options.work_scheduler.clone())
                    .0;
                *sc_out_clone.lock().unwrap() = Some(sc.clone());

                #[derive(Debug)]
                struct MockPicker {
                    sc: Arc<dyn Subchannel>,
                }
                impl Picker for MockPicker {
                    fn pick(&self, _req: &RequestHeaders) -> PickResult {
                        PickResult::Pick(Pick {
                            subchannel: self.sc.clone(),
                            metadata: MetadataMap::new(),
                            on_complete: None,
                        })
                    }
                }

                cc.update_picker(LbState {
                    connectivity_state: ConnectivityState::Ready,
                    picker: Arc::new(MockPicker { sc }),
                });
            })),
            ..Default::default()
        });

        env.policy.work(None, &mut env.tcc);
        env.expect_new_subchannel();
        let state = env.expect_picker_update();

        let req = new_request_headers();
        let result = state.picker.pick(&req);
        let PickResult::Pick(pick) = result else {
            panic!("expected Pick")
        };

        let external_sc = sc_out.lock().unwrap().take().unwrap();
        let shared = external_sc.downcast_ref::<SharedSubchannel>().unwrap();

        assert!(Arc::ptr_eq(&pick.subchannel, &shared.delegate));
    }

    // Tests that update/work/exit_idle methods are delegated appropriately and
    // request_resolution is delegated back to the channel.
    #[test]
    fn test_delegates_other_methods() {
        let called = Arc::new(Mutex::new(vec![]));

        let mut env = new_env(StubPolicyFuncs {
            resolver_update: Some(Arc::new({
                let called_clone = called.clone();
                move |_data, _update, _config, _cc| {
                    called_clone.lock().unwrap().push("resolver_update");
                    Ok(())
                }
            })),
            work: Some(Arc::new({
                let called_clone = called.clone();
                move |_data, _workitem, cc| {
                    called_clone.lock().unwrap().push("work");
                    cc.request_resolution();
                }
            })),
            exit_idle: Some(Arc::new({
                let called_clone = called.clone();
                move |_data, _cc| called_clone.lock().unwrap().push("exit_idle")
            })),
        });

        let update = ResolverUpdate::default();
        env.policy
            .resolver_update(update, &(Arc::new(()) as DynLbConfig), &mut env.tcc)
            .unwrap();
        env.policy.work(None, &mut env.tcc);
        env.policy.exit_idle(&mut env.tcc);

        assert_eq!(
            *called.lock().unwrap(),
            vec!["resolver_update", "work", "exit_idle"]
        );

        env.expect_request_resolution();
    }

    // Tests that a shared subchannel's correct state is returned by
    // new_subchannel.
    #[test]
    fn test_new_subchannel_state() {
        type WorkFn = Box<dyn FnOnce(&mut dyn ChannelController, Arc<dyn WorkScheduler>) + Send>;
        let (tx_work, rx_work) = mpsc::channel::<WorkFn>();
        // Wrap rx_work in a mutex to allow the stub work Fn() closure to access
        // it mutably.
        let rx_work = Arc::new(Mutex::new(rx_work));
        let rx_work_clone = rx_work.clone();

        let mut env = new_env(StubPolicyFuncs {
            work: Some(Arc::new(move |data, work_item, cc| {
                // Ignore subchannel state updates; they must not be routed to the
                // work func, which expects an entry in rx_work.
                if let Some(Ok(_)) = work_item.map(|d| d.downcast::<SubchannelUpdate>()) {
                    return;
                }
                let work_scheduler = data.lb_policy_options.work_scheduler.clone();
                (rx_work_clone.lock().unwrap().recv().unwrap())(cc, work_scheduler);
            })),
            ..Default::default()
        });

        let addr = Address {
            address: "127.0.0.2:80".to_string().into(),
            ..Default::default()
        };

        let sc1 = Arc::new(Mutex::new(None));

        // Create the first subchannel
        let sc1_clone = sc1.clone();
        let addr_clone = addr.clone();
        tx_work
            .send(Box::new(move |cc, work_scheduler| {
                let (sc, state) = cc.new_subchannel(&addr_clone, work_scheduler);
                assert_eq!(state.connectivity_state, ConnectivityState::Idle);
                *sc1_clone.lock().unwrap() = Some(sc);
            }))
            .unwrap();
        env.policy.work(None, &mut env.tcc);
        let int_sc = env.expect_new_subchannel();

        // Update the state to Connecting.
        env.send_subchannel_update(&int_sc, &SubchannelState::connecting());
        env.run_work();

        // Create a second subchannel for the address and verify that the state
        // is also Connecting.
        let addr_clone = addr.clone();
        tx_work
            .send(Box::new(move |cc, work_scheduler| {
                let (_sc, state) = cc.new_subchannel(&addr_clone, work_scheduler);
                assert_eq!(state.connectivity_state, ConnectivityState::Connecting);
            }))
            .unwrap();
        env.policy.work(None, &mut env.tcc); // execute the work above
        env.run_work(); // When _sc is dropped a work item is produced; run it.

        // Update the state to Ready.
        env.send_subchannel_update(&int_sc, &SubchannelState::ready());
        env.run_work();

        // Create another subchannel for the address and verify that the state
        // is now Ready.
        let addr_clone = addr.clone();
        tx_work
            .send(Box::new(move |cc, work_scheduler| {
                let (_sc, state) = cc.new_subchannel(&addr_clone, work_scheduler);
                assert_eq!(state.connectivity_state, ConnectivityState::Ready);
            }))
            .unwrap();
        env.policy.work(None, &mut env.tcc);

        assert!(
            rx_work.lock().unwrap().try_recv().is_err(),
            "not all work functions were executed"
        );
    }

    // A channel controller that reports a state change for every subchannel it
    // creates, synchronously, from inside new_subchannel. This tests that a
    // policy can handle an eager update before new_subchannel returns.
    struct EagerUpdateChannelController {
        tx_events: mpsc::Sender<TestEvent>,
    }

    impl ChannelController for EagerUpdateChannelController {
        fn new_subchannel(
            &mut self,
            address: &Address,
            work_scheduler: Arc<dyn WorkScheduler>,
        ) -> (Arc<dyn Subchannel>, SubchannelState) {
            let subchannel: Arc<dyn Subchannel> =
                Arc::new(TestSubchannel::new_with_work_scheduler(
                    address.clone(),
                    self.tx_events.clone(),
                    work_scheduler.clone(),
                ));
            self.tx_events
                .send(TestEvent::NewSubchannel(subchannel.clone()))
                .unwrap();
            // Report the initial state before returning.
            work_scheduler.schedule_work(Some(Box::new(SubchannelUpdate::new(
                subchannel.clone(),
                SubchannelState::idle(),
            ))));
            (subchannel, SubchannelState::idle())
        }

        fn update_picker(&mut self, update: LbState) {
            self.tx_events
                .send(TestEvent::UpdatePicker(update))
                .unwrap();
        }

        fn request_resolution(&mut self) {
            self.tx_events.send(TestEvent::RequestResolution).unwrap();
        }
    }

    // Tests that a state reported by the channel from inside new_subchannel,
    // i.e. before the subchannel has been registered, is still delivered to the
    // policy that created it.
    #[test]
    fn test_update_during_new_subchannel_is_delivered() {
        let (tx_events, rx_events) = mpsc::channel();
        let (tx_work, rx_work) = mpsc::channel();
        let mut cc = EagerUpdateChannelController { tx_events };

        let updates = Arc::new(Mutex::new(Vec::new()));
        let updates_clone = updates.clone();
        let sc_out = Arc::new(Mutex::new(None));
        let sc_out_clone = sc_out.clone();

        let work_scheduler = Arc::new(TestWorkScheduler { tx_work });
        let mock = StubPolicy::new(
            StubPolicyFuncs {
                work: Some(Arc::new(move |data, work_item, cc| {
                    if let Some(Ok(update)) = work_item.map(|d| d.downcast::<SubchannelUpdate>()) {
                        updates_clone
                            .lock()
                            .unwrap()
                            .push(update.state.connectivity_state);
                        return;
                    }
                    let addr = Address {
                        address: "127.0.0.1:80".to_string().into(),
                        ..Default::default()
                    };
                    // The returned subchannel must be kept alive: the fan-out
                    // only delivers to external subchannels that still exist.
                    *sc_out_clone.lock().unwrap() = Some(
                        cc.new_subchannel(&addr, data.lb_policy_options.work_scheduler.clone())
                            .0,
                    );
                })),
                ..Default::default()
            },
            LbPolicyOptions {
                work_scheduler: work_scheduler.clone(),
                runtime: default_runtime(),
            },
        );

        let mut sharing = SubchannelSharing::new(mock, work_scheduler);

        // Creates the subchannel; the controller reports its state before
        // new_subchannel returns.
        sharing.work(None, &mut cc);

        // That update must not have been dropped.
        run_pending_work(&mut sharing, &rx_work, &mut cc);
        assert_eq!(*updates.lock().unwrap(), vec![ConnectivityState::Idle]);

        // Running the work consumes only work items, so the subchannel
        // creation is still observable afterwards.
        assert!(matches!(
            rx_events.try_recv(),
            Ok(TestEvent::NewSubchannel(_))
        ));
    }
}
