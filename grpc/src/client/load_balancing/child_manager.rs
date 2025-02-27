/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 *
 */

//! A utility which helps parent LB policies manage multiple children for the
//! purposes of forwarding channel updates.

// TODO: This is mainly provided as a fairly complex example of the current LB
// policy in use.  Complete tests must be written before it can be used in
// production.  Also, support for the work scheduler is missing.

use std::{collections::HashMap, error::Error, hash::Hash, mem, sync::Arc};

use crate::client::load_balancing::{
    ChannelController, LbConfig, LbPolicy, LbPolicyBuilder, LbPolicyOptions, LbState, WorkScheduler,
};
use crate::client::name_resolution::{Address, ResolverUpdate};

use super::{Subchannel, SubchannelState};

// An LbPolicy implementation that manages multiple children.
pub struct ChildManager<T> {
    subchannel_child_map: HashMap<Subchannel, usize>,
    children: Vec<Child<T>>,
    shard_update: Box<ResolverUpdateSharder<T>>,
}

struct Child<T> {
    identifier: T,
    policy: Box<dyn LbPolicy>,
    state: LbState,
}

/// A collection of data sent to a child of the ChildManager.
pub struct ChildUpdate<T> {
    /// The identifier the ChildManager should use for this child.
    pub child_identifier: T,
    /// The builder the ChildManager should use to create this child if it does
    /// not exist.
    pub child_policy_builder: Box<dyn LbPolicyBuilder>,
    /// The relevant ResolverUpdate to send to this child.
    pub child_update: ResolverUpdate,
}

// TODO: convert to a trait?
/// Performs the operation of sharding an aggregate ResolverUpdate into one or
/// more ChildUpdates.  Called automatically by the ChildManager when its
/// resolver_update method is called.
pub type ResolverUpdateSharder<T> =
    fn(
        ResolverUpdate,
    ) -> Result<Box<dyn Iterator<Item = ChildUpdate<T>>>, Box<dyn Error + Send + Sync>>;

impl<T: PartialEq + Hash + Eq> ChildManager<T> {
    /// Creates a new ChildManager LB policy.  shard_update is called whenever a
    /// resolver_update operation occurs.
    pub fn new(shard_update: Box<ResolverUpdateSharder<T>>) -> Self {
        Self {
            subchannel_child_map: HashMap::default(),
            children: Vec::default(),
            shard_update,
        }
    }

    /// Returns data for all current children.
    pub fn child_states(&mut self) -> impl Iterator<Item = (&T, &LbState)> {
        self.children
            .iter()
            .map(|child| (&child.identifier, &child.state))
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
            self.subchannel_child_map.insert(csc, child_idx);
        }
        // Update the tracked state if the child produced an update.
        if let Some(state) = channel_controller.picker_update {
            self.children[child_idx].state = state;
        };
    }
}

impl<T: PartialEq + Hash + Eq + Send> LbPolicy for ChildManager<T> {
    fn resolver_update(
        &mut self,
        resolver_update: ResolverUpdate,
        config: Option<&LbConfig>,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // First determine if the incoming update is valid.
        let child_updates = (self.shard_update)(resolver_update)?;

        // Replace self.children with an empty vec.
        let mut old_children = vec![];
        mem::swap(&mut self.children, &mut old_children);

        // Replace the subchannel map with an empty map.
        let mut old_subchannel_child_map = HashMap::new();
        mem::swap(
            &mut self.subchannel_child_map,
            &mut old_subchannel_child_map,
        );
        // Reverse the old subchannel map.
        let mut old_child_subchannels_map: HashMap<usize, Vec<Subchannel>> = HashMap::new();
        for (subchannel, child_idx) in old_subchannel_child_map {
            old_child_subchannels_map
                .entry(child_idx)
                .or_insert_with(|| vec![])
                .push(subchannel);
        }

        // Build a map of the old children from their IDs for efficient lookups.
        let old_children = old_children
            .into_iter()
            .enumerate()
            .map(|(old_idx, e)| (e.identifier, (e.policy, e.state, old_idx)));
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
            if let Some((policy, state, old_idx)) = old_children.remove(&identifier) {
                for subchannel in old_child_subchannels_map
                    .remove(&old_idx)
                    .into_iter()
                    .flatten()
                {
                    self.subchannel_child_map.insert(subchannel, new_idx);
                }
                self.children.push(Child {
                    identifier,
                    state,
                    policy,
                });
            } else {
                let policy = builder.build(LbPolicyOptions {
                    work_scheduler: Arc::new(UnimplWorkScheduler {}),
                });
                let state = LbState::initial();
                self.children.push(Child {
                    identifier,
                    state,
                    policy,
                });
            };
        }

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
        subchannel: &Subchannel,
        state: &SubchannelState,
        channel_controller: &mut dyn ChannelController,
    ) {
        // Determine which child created this subchannel.
        let child_idx = *self.subchannel_child_map.get(subchannel).unwrap();
        let policy = &mut self.children[child_idx].policy;
        // Wrap the channel_controller to track the child's operations.
        let mut channel_controller = WrappedController::new(channel_controller);
        // Call the proper child.
        policy.subchannel_update(subchannel, state, &mut channel_controller);
        self.resolve_child_controller(channel_controller, child_idx);
    }

    fn work(&mut self, _channel_controller: &mut dyn ChannelController) {
        todo!();
    }
}

struct WrappedController<'a> {
    channel_controller: &'a mut dyn ChannelController,
    created_subchannels: Vec<Subchannel>,
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

impl<'a> ChannelController for WrappedController<'a> {
    fn new_subchannel(&mut self, address: &Address) -> Subchannel {
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

pub struct UnimplWorkScheduler;

impl WorkScheduler for UnimplWorkScheduler {
    fn schedule_work(&self) {
        todo!();
    }
}
