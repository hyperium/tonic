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
use std::{
    fmt::Debug,
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::client::load_balancing::{
<<<<<<< HEAD
    ChannelController, LbConfig, LbPolicy, LbPolicyBuilder, LbPolicyOptions, LbState,
    WeakSubchannel, WorkScheduler,
=======
    ChannelController, ConnectivityState, ExternalSubchannel, Failing, LbConfig, LbPolicy,
    LbPolicyBuilder, LbPolicyOptions, LbState, ParsedJsonLbConfig, PickResult, Picker,
    QueuingPicker, Subchannel, SubchannelState, WeakSubchannel, WorkScheduler, GLOBAL_LB_REGISTRY,
>>>>>>> f7537e6 (fixed some logic for review)
};
use crate::client::name_resolution::{Address, ResolverUpdate};

use super::{Subchannel, SubchannelState};

// An LbPolicy implementation that manages multiple children.
pub struct ChildManager<T> {
    subchannel_child_map: HashMap<WeakSubchannel, usize>,
    children: Vec<Child<T>>,
    update_sharder: Box<dyn ResolverUpdateSharder<T>>,
    pending_work: Arc<Mutex<HashSet<usize>>>,
<<<<<<< HEAD
}

=======
    updated: bool, // true if a child has updated its state since the last call to has_updated.
    prev_state: ConnectivityState,
    last_ready_pickers: Vec<Arc<dyn Picker>>,
}

pub trait ChildIdentifier: PartialEq + Hash + Eq + Send + Sync + Debug + 'static {}

>>>>>>> f7537e6 (fixed some logic for review)
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
    pub fn new(update_sharder: Box<dyn ResolverUpdateSharder<T>>) -> Self {
        Self {
            update_sharder,
            subchannel_child_map: Default::default(),
            children: Default::default(),
            pending_work: Default::default(),
<<<<<<< HEAD
=======
            updated: false,
            prev_state: ConnectivityState::Idle,
            last_ready_pickers: Vec::new(),
>>>>>>> f7537e6 (fixed some logic for review)
        }
    }

    /// Returns data for all current children.
    pub fn child_states(&mut self) -> impl Iterator<Item = (&T, &LbState)> {
        self.children
            .iter()
            .map(|child| (&child.identifier, &child.state))
    }

<<<<<<< HEAD
=======
    /// Returns true if a child has produced an update and resets flag to false.
    pub fn has_updated(&mut self) -> bool {
        mem::take(&mut self.updated)
    }

>>>>>>> f7537e6 (fixed some logic for review)
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
<<<<<<< HEAD
        };
=======
            self.updated = true;
        };
    }

    /// Called to aggregate states from children policies then returns a update.
    pub fn aggregate_states(&mut self) -> Option<LbState> {
        let current_connectivity_state = self.prev_state.clone();
        let child_states_vec = self.child_states();

        // Construct pickers to return.
        let mut ready_pickers = RoundRobinPicker::new();

        let mut has_connecting = false;
        let mut has_ready = false;
        let mut is_transient_failure = true;

        for (child_id, state) in child_states_vec {
            match state.connectivity_state {
                ConnectivityState::Idle => {
                    has_connecting = true;
                    is_transient_failure = false;
                }
                ConnectivityState::Connecting => {
                    has_connecting = true;
                    is_transient_failure = false;
                }
                ConnectivityState::Ready => {
                    ready_pickers.add_picker(state.picker.clone());
                    is_transient_failure = false;
                    has_ready = true;
                }
                _ => {}
            }
        }

        // Decide the new aggregate state.
        let new_state = if has_ready {
            ConnectivityState::Ready
        } else if has_connecting {
            ConnectivityState::Connecting
        } else if is_transient_failure {
            ConnectivityState::TransientFailure
        } else {
            ConnectivityState::Connecting
        };

        // Now update state and send picker as appropriate.
        match new_state {
            ConnectivityState::Ready => {
                let pickers_vec = ready_pickers.pickers.clone();
                let picker: Arc<dyn Picker> = Arc::new(ready_pickers);
                let should_update =
                    !self.compare_prev_to_new_pickers(&self.last_ready_pickers, &pickers_vec);

                if should_update || self.prev_state != ConnectivityState::Ready {
                    self.prev_state = ConnectivityState::Ready;
                    self.last_ready_pickers = pickers_vec;
                    return Some(LbState {
                        connectivity_state: ConnectivityState::Ready,
                        picker,
                    });
                } else {
                    return None;
                }
            }
            ConnectivityState::Connecting => {
                if self.prev_state == ConnectivityState::TransientFailure
                    && new_state != ConnectivityState::Ready
                {
                    return None;
                }
                if self.prev_state != ConnectivityState::Connecting {
                    let picker = Arc::new(QueuingPicker {});
                    self.prev_state = ConnectivityState::Connecting;
                    return Some(LbState {
                        connectivity_state: ConnectivityState::Connecting,
                        picker,
                    });
                } else {
                    return None;
                }
            }
            ConnectivityState::Idle => {
                let picker = Arc::new(QueuingPicker {});
                self.prev_state = ConnectivityState::Connecting;
                return Some(LbState {
                    connectivity_state: ConnectivityState::Connecting,
                    picker,
                });
            }
            ConnectivityState::TransientFailure => {
                if current_connectivity_state != ConnectivityState::TransientFailure {
                    self.prev_state = ConnectivityState::TransientFailure;
                    let picker = Arc::new(Failing {
                        error: "No children available".to_string(),
                    });
                    return Some(LbState {
                        connectivity_state: ConnectivityState::TransientFailure,
                        picker: picker,
                    });
                } else {
                    return None;
                }
            }
        }
    }
}

impl<T: ChildIdentifier> ChildManager<T> {
    fn compare_prev_to_new_pickers(
        &self,
        old_pickers: &[Arc<dyn Picker>],
        new_pickers: &[Arc<dyn Picker>],
    ) -> bool {
        // If length is different, then definitely not the same picker.
        if old_pickers.len() != new_pickers.len() {
            return false;
        }
        // Compares two vectors of pickers by pointer equality and returns true if all pickers are the same.
        for (x, y) in old_pickers.iter().zip(new_pickers.iter()) {
            if !Arc::ptr_eq(x, y) {
                return false;
            }
        }
        true
>>>>>>> f7537e6 (fixed some logic for review)
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

<<<<<<< HEAD
    fn exit_idle(&mut self, _channel_controller: &mut dyn ChannelController) {
        todo!("implement exit_idle")
=======
    fn exit_idle(&mut self, channel_controller: &mut dyn ChannelController) {
        let child_idxes = mem::take(&mut *self.pending_work.lock().unwrap());
        for child_idx in child_idxes {
            let mut channel_controller = WrappedController::new(channel_controller);
            self.children[child_idx]
                .policy
                .exit_idle(&mut channel_controller);
            self.resolve_child_controller(channel_controller, child_idx);
        }
>>>>>>> f7537e6 (fixed some logic for review)
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
<<<<<<< HEAD
        self.picker_update = Some(update);
=======
        self.picker_update = Some(update.clone());
>>>>>>> f7537e6 (fixed some logic for review)
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
<<<<<<< HEAD
=======

struct RoundRobinPicker {
    pickers: Vec<Arc<dyn Picker>>,
    next: AtomicUsize,
}

impl RoundRobinPicker {
    fn new() -> Self {
        Self {
            pickers: vec![],
            next: AtomicUsize::new(0),
        }
    }

    fn add_picker(&mut self, picker: Arc<dyn Picker>) {
        self.pickers.push(picker);
    }
}

impl Picker for RoundRobinPicker {
    fn pick(&self, request: &Request) -> PickResult {
        let len = self.pickers.len();
        if len == 0 {
            return PickResult::Queue;
        }
        let idx = self.next.fetch_add(1, Ordering::Relaxed) % len;
        self.pickers[idx].pick(request)
    }
}
>>>>>>> f7537e6 (fixed some logic for review)
