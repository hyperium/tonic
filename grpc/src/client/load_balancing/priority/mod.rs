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

//! Priority Load Balancing Policy (`priority_experimental`).
//!
//! This module implements the `priority_experimental` load balancing policy as
//! specified in [gRFC A56: `priority_experimental` LB policy] and updated by
//! [gRFC A115: disable Priority LB policy child policy retention cache].
//!
//! # Overview
//!
//! The priority LB policy manages an ordered list of child policies. It routes
//! RPCs to the highest-priority child that is reachable (in `READY` or `IDLE`
//! state). If higher-priority children are unavailable, fail, or take too long
//! to connect, the policy fails over to lower-priority children.
//!
//! Each endpoint in a [`ResolverUpdate`] delivered to the priority LB must be
//! annotated with a hierarchical path attribute
//! (see [gRFC A56: Hierarchical Addresses]), otherwise, it will be ignored.
//!
//! [gRFC A56: `priority_experimental` LB policy]:
//!   https://github.com/grpc/proposal/blob/master/A56-priority-lb-policy.md
//! [gRFC A56: Hierarchical Addresses]:
//!   https://github.com/grpc/proposal/blob/master/A56-priority-lb-policy.md#hierarchical-addresses
//! [gRFC A115: disable Priority LB policy child policy retention cache]:
//!   https://github.com/grpc/proposal/blob/master/A115-remove-priority-lb-child-policy-cache.md

use std::collections::HashMap;
use std::fmt;
use std::mem;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::client::ConnectivityState;
use crate::client::load_balancing::ChannelController;
use crate::client::load_balancing::FailingPicker;
use crate::client::load_balancing::GLOBAL_LB_REGISTRY;
use crate::client::load_balancing::LbConfigJson;
use crate::client::load_balancing::LbPolicy;
use crate::client::load_balancing::LbPolicyBuilder;
use crate::client::load_balancing::LbPolicyOptions;
use crate::client::load_balancing::LbState;
use crate::client::load_balancing::ParsedLbConfig;
use crate::client::load_balancing::WorkData;
use crate::client::load_balancing::WorkScheduler;
use crate::client::load_balancing::child_manager::Child;
use crate::client::load_balancing::endpoint_filtering;
use crate::client::load_balancing::graceful_switch::GracefulSwitchLbConfig;
use crate::client::load_balancing::graceful_switch::GracefulSwitchPolicy;
use crate::client::name_resolution::ResolverUpdate;
use crate::rt::BoxedTaskHandle;
use crate::rt::GrpcRuntime;

/// The name under which the builder is registered in the global LB registry.
pub static POLICY_NAME: &str = "priority_experimental";

/// Failover timeout for a child attempting to connect (10 seconds).
const CONNECTING_TIMEOUT: Duration = Duration::from_secs(10);

/// Retention timeout for deactivated lower-priority children (15 minutes).
const DEACTIVATION_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Registers the `priority_experimental` LB policy builder in the global LB
/// registry.
pub fn reg() {
    GLOBAL_LB_REGISTRY.add_builder(Builder {});
}

/// Parsed configuration for the `priority_experimental` load balancing policy.
///
/// Corresponds to `PriorityLoadBalancingPolicyConfig` defined in
/// [gRFC A56 (Section LB Policy Configuration)]:
///
/// ```proto
/// message PriorityLoadBalancingPolicyConfig {
///   map<string, Child> children = 1;
///   repeated string priorities = 2;
/// }
/// ```
///
/// [gRFC A56 (Section LB Policy Configuration)]:
///   https://github.com/grpc/proposal/blob/master/A56-priority-lb-policy.md#lb-policy-configuration
#[derive(Debug, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PriorityConfig {
    /// Ordered list of child balancer names in decreasing priority order
    /// (index 0 is highest priority).
    priorities: Vec<String>,

    /// Map from child balancer names to their configurations.
    ///
    /// Names correspond to entries in [`priorities`]. Decoupling names from
    /// priority positions allows existing children to be moved between
    /// priorities without recreating the child policy and its subchannels.
    children: HashMap<String, PriorityChildConfig>,
}

/// Configuration for an individual child under the `priority_experimental`
/// LB policy.
///
/// Corresponds to the protobuf message
/// `PriorityLoadBalancingPolicyConfig.Child` defined in
/// [gRFC A56 (Section LB Policy Configuration)]:
///
/// ```proto
/// message Child {
///   repeated LoadBalancingConfig config = 1;
///   bool ignore_reresolution_requests = 2;
/// }
/// ```
///
/// [gRFC A56 (Section LB Policy Configuration)]:
///   https://github.com/grpc/proposal/blob/master/A56-priority-lb-policy.md#lb-policy-configuration
#[derive(Debug, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PriorityChildConfig {
    /// The child load balancing policy configuration, specifying the policy
    /// to instantiate (e.g., `round_robin`, `pick_first`, `weighted_target`)
    /// and its policy-specific configuration.
    config: ParsedLbConfig,

    /// If `true`, re-resolution requests from this child policy will be ignored
    /// and not forwarded to the channel controller.
    ///
    /// This prevents lower-priority or failover children from triggering
    /// unnecessary name resolution when they enter `TRANSIENT_FAILURE`.
    /// See [gRFC A37] and [gRFC A56] for details.
    ///
    /// [gRFC A37]:
    ///   https://github.com/grpc/proposal/blob/master/A37-xds-aggregate-and-logical-dns-clusters.md
    /// [gRFC A56]:
    ///   https://github.com/grpc/proposal/blob/master/A56-priority-lb-policy.md
    #[serde(default)]
    ignore_reresolution_requests: bool,
}

impl PriorityConfig {
    fn validate(&self) -> Result<(), String> {
        for name in &self.priorities {
            if !self.children.contains_key(name) {
                return Err(format!(
                    "LB policy name \"{name}\" found in Priorities field ({:?}) is not found in Children field ({:?})",
                    self.priorities, self.children
                ));
            }
        }
        for name in self.children.keys() {
            if !self.priorities.contains(name) {
                return Err(format!(
                    "LB policy name \"{name}\" found in Children field ({:?}) is not found in Priorities field ({:?})",
                    self.children, self.priorities
                ));
            }
        }
        Ok(())
    }

    /// Returns the configured children ordered from the highest priority to
    /// the lowest.
    fn ordered_children(&self) -> impl Iterator<Item = (&String, &PriorityChildConfig)> {
        self.priorities
            .iter()
            .filter_map(|name| self.children.get(name).map(|config| (name, config)))
    }
}

/// Internal tracking data and state for a configured priority child.
#[derive(Debug)]
struct ChildData {
    /// The name identifying this child in the LB config.
    name: String,
    /// The current LB configuration for this child.
    child_config: PriorityChildConfig,
    /// The latest name resolver update received for this child.
    latest_update: ResolverUpdate,
    /// The child policy and its lifecycle state, or `None` if the child has
    /// not been created yet, or was deleted after its deactivation timer
    /// expired.
    ///
    /// Children are created lazily when evaluated during priority selection
    /// using `latest_update` (see [gRFC A56 (Section Child Lifetime
    /// Management)]).
    ///
    /// [gRFC A56 (Section Child Lifetime Management)]:
    ///   https://github.com/grpc/proposal/blob/master/A56-priority-lb-policy.md#child-lifetime-management
    child: Option<CreatedChild>,
}

impl ChildData {
    /// Returns the lifecycle state of the child, or `None` if it has not been
    /// created.
    #[cfg(test)]
    fn state(&self) -> Option<&ChildState> {
        self.child.as_ref().map(|child| &child.state)
    }
}

/// A child policy that has been created, along with its lifecycle state.
#[derive(Debug)]
struct CreatedChild {
    /// The child policy.  Its most recently reported [`LbState`] is available
    /// via [`Child::state`].
    policy: Child<GracefulSwitchPolicy>,
    /// The lifecycle state of the child, derived from the connectivity states
    /// reported by `policy` and the timers.
    state: ChildState,
}

/// Internal work item scheduled by [`PriorityPolicy`] timers upon expiration.
#[derive(Debug)]
struct PriorityTimerWork;

/// An RAII timer that triggers a work notification upon expiration.
///
/// When instantiated via [`Timer::new`], a background task is spawned on the
/// runtime that sleeps for the specified duration, marks the timer as expired
/// and then invokes [`WorkScheduler::schedule_work`] with
/// [`PriorityTimerWork`].
///
/// Expiry is tracked with a flag set by the background task. This avoids
/// depending on a specific time source and guarantees that the timer is
/// observed as expired whenever the work it schedules runs.
///
/// If dropped before expiration (e.g., when a child transitions out of
/// `Connecting` or is reactivated from `Deactivated`), the spawned task is
/// aborted via its task handle, preventing stale timer wakeups.
struct Timer {
    /// Set by the background task once the sleep completes, before work is
    /// scheduled.
    expired: Arc<AtomicBool>,
    /// Task handle for the background sleep task, aborted upon drop.
    task_handle: BoxedTaskHandle,
}

impl fmt::Debug for Timer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Timer")
            .field("expired", &self.expired())
            .finish()
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        self.task_handle.abort();
    }
}

impl Timer {
    /// Spawns a new timer for the given duration that schedules work on
    /// completion.
    fn new(duration: Duration, work_scheduler: Arc<dyn WorkScheduler>, rt: GrpcRuntime) -> Timer {
        let rt_clone = rt.clone();
        let expired = Arc::new(AtomicBool::new(false));
        let expired_clone = expired.clone();
        let task_handle = rt.spawn(Box::pin(async move {
            rt_clone.sleep(duration).await;
            expired_clone.store(true, Ordering::Release);
            work_scheduler.schedule_work(Some(Box::new(PriorityTimerWork)));
        }));
        Timer {
            expired,
            task_handle,
        }
    }

    /// Returns true once the timer's duration has elapsed.
    fn expired(&self) -> bool {
        self.expired.load(Ordering::Acquire)
    }
}

/// Lifecycle and connectivity states of a child policy under
/// `priority_experimental`.
///
/// The child's most recently reported [`LbState`] is not stored here; it is
/// available via [`Child::state`].
///
/// [gRFC A56 (Section Child Lifetime Management)]:
///   https://github.com/grpc/proposal/blob/master/A56-priority-lb-policy.md#child-lifetime-management
/// [gRFC A56 (Section Child Connectivity State Tracking)]:
///   https://github.com/grpc/proposal/blob/master/A56-priority-lb-policy.md#child-connectivity-state-tracking
#[derive(Debug)]
enum ChildState {
    /// The child is actively attempting to connect, with a 10-second failover
    /// timer running.
    ///
    /// While this timer is active, priority selection will wait for this child
    /// before evaluating lower priorities (see
    /// [gRFC A56 (Section Child Connectivity State Tracking)]).
    Connecting(Timer),

    /// The child is in `CONNECTING` state, but its 10-second failover timer has
    /// expired.
    ///
    /// The child continues attempting connection in the background, but
    /// priority selection may now proceed to check lower priorities (see
    /// [gRFC A56 (Section Child Connectivity State Tracking)]).
    ConnectingExpired,

    /// The child reported `TRANSIENT_FAILURE`.
    ///
    /// The failover timer is cancelled, and priority selection may proceed to
    /// check lower priorities.
    TransientFailure,

    /// The child is in `READY` or `IDLE` state.
    ///
    /// When a child reaches this state, it is selected as the active priority
    /// and lower-priority children are deactivated (with a 15-minute timer).
    ReadyOrIdle,

    /// The child was previously active, but has been superseded by a
    /// higher-priority child reaching `ReadyOrIdle` state.
    ///
    /// A 15-minute deactivation timer is running ([`DEACTIVATION_TIMEOUT`]).
    /// If higher priorities fail before this timer expires, the child will be
    /// reactivated immediately without connection churn. If the timer expires,
    /// the child is destroyed (see
    /// [gRFC A56 (Section Child Lifetime Management)]).
    ///
    /// Per [gRFC A115], only children still configured in `PriorityConfig`
    /// enter this state; unconfigured children are removed immediately.
    ///
    /// [gRFC A115]:
    ///   https://github.com/grpc/proposal/blob/master/A115-remove-priority-lb-child-policy-cache.md
    Deactivated(Timer),
}

impl ChildState {
    /// Returns the state of a child that is reporting `connectivity_state`,
    /// disregarding its previous state.  Starts a failover timer if the child
    /// is connecting.
    fn from_connectivity_state(
        connectivity_state: ConnectivityState,
        work_scheduler: &Arc<dyn WorkScheduler>,
        rt: &GrpcRuntime,
    ) -> Self {
        match connectivity_state {
            ConnectivityState::Idle | ConnectivityState::Ready => ChildState::ReadyOrIdle,
            ConnectivityState::Connecting => ChildState::Connecting(Timer::new(
                CONNECTING_TIMEOUT,
                work_scheduler.clone(),
                rt.clone(),
            )),
            ConnectivityState::TransientFailure => ChildState::TransientFailure,
        }
    }

    /// Updates the state of a child in this state that is now reporting
    /// `connectivity_state`.  Starts a failover timer if the child starts
    /// connecting.
    fn update(
        &mut self,
        connectivity_state: ConnectivityState,
        work_scheduler: &Arc<dyn WorkScheduler>,
        rt: &GrpcRuntime,
    ) {
        match (&*self, connectivity_state) {
            // While deactivated, retain the 15-minute deactivation timer (see
            // gRFC A56 (Section Child Lifetime Management)).
            (ChildState::Deactivated(_), _) => {}
            // Keep the failover timer that is already running, unless it has
            // expired.
            (ChildState::Connecting(timer), ConnectivityState::Connecting) => {
                if timer.expired() {
                    *self = ChildState::ConnectingExpired;
                }
            }
            // A child that has failed, or whose failover timer has expired, is
            // not given another failover timer until it becomes READY or IDLE.
            (
                ChildState::ConnectingExpired | ChildState::TransientFailure,
                ConnectivityState::Connecting,
            ) => *self = ChildState::ConnectingExpired,
            (_, connectivity_state) => {
                *self = Self::from_connectivity_state(connectivity_state, work_scheduler, rt);
            }
        }
    }
}

/// LB policy builder for the `priority_experimental` load balancing policy.
#[derive(Debug)]
struct Builder {}

impl LbPolicyBuilder for Builder {
    type LbPolicy = PriorityPolicy;

    fn build(&self, options: LbPolicyOptions) -> Self::LbPolicy {
        let rt = options.runtime;
        PriorityPolicy {
            children: Vec::default(),
            current_priority: None,
            published_lb_state: None,
            rt,
            work_scheduler: options.work_scheduler,
        }
    }

    fn name(&self) -> &'static str {
        POLICY_NAME
    }

    fn parse_config(&self, config: &LbConfigJson) -> Result<PriorityConfig, String> {
        let cfg: PriorityConfig = config.convert_to().map_err(|e| e.to_string())?;
        cfg.validate()?;
        Ok(cfg)
    }
}

/// The `priority_experimental` load balancing policy instance.
///
/// Manages a prioritized collection of child policies.
#[derive(Debug)]
struct PriorityPolicy {
    /// Current priority hierarchy: the configured children, ordered from the
    /// highest priority (index 0) to the lowest.
    children: Vec<ChildData>,
    /// The index in `children` of the currently selected priority, or `None`
    /// if there are no children.
    ///
    /// Recomputed by [`choose_priority`](Self::choose_priority) at the end of
    /// every operation.
    current_priority: Option<usize>,
    /// The most recent LB state published to the channel controller.
    ///
    /// Used to debounce redundant picker updates when internal priority or
    /// child events do not change the active picker.
    published_lb_state: Option<LbState>,
    rt: GrpcRuntime,
    work_scheduler: Arc<dyn WorkScheduler>,
}

impl LbPolicy for PriorityPolicy {
    type LbConfig = PriorityConfig;

    fn resolver_update(
        &mut self,
        update: ResolverUpdate,
        config: &Self::LbConfig,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), String> {
        let mut sharded_endpoints = update.endpoints.map(endpoint_filtering::group_by_path);

        // Index the existing children by name so they can be moved into the
        // new priority order.  Children that are no longer configured are left
        // behind in this map and dropped, see gRFC A115.
        let mut old_children: HashMap<String, ChildData> = mem::take(&mut self.children)
            .into_iter()
            .map(|child_data| (child_data.name.clone(), child_data))
            .collect();

        for (name, child_cfg) in config.ordered_children() {
            let endpoints = match &mut sharded_endpoints {
                Ok(grouped) => Ok(grouped.remove(name).unwrap_or_default()),
                Err(status) => Err(status.clone()),
            };

            let resolver_update = ResolverUpdate {
                attributes: update.attributes.clone(),
                endpoints,
                service_config: update.service_config.clone(),
                resolution_note: update.resolution_note.clone(),
            };

            let child_data = match old_children.remove(name) {
                Some(mut child_data) => {
                    child_data.child_config = child_cfg.clone();
                    child_data.latest_update = resolver_update;
                    child_data
                }
                None => ChildData {
                    name: name.clone(),
                    child_config: child_cfg.clone(),
                    latest_update: resolver_update,
                    child: None,
                },
            };
            self.children.push(child_data);
        }

        debug_assert!(
            sharded_endpoints.is_err() || sharded_endpoints.as_ref().unwrap().is_empty(),
            "endpoints contain paths not belonging to any child: {:?}",
            sharded_endpoints
        );

        // Only children that have already been created are updated; the
        // remaining ones are created lazily by choose_priority.  As specified
        // in gRFC A56 (Section Configuration Updates), priority re-evaluation
        // is deferred until all child updates have been applied.
        let mut errs = vec![];
        for child_data in &mut self.children {
            let Some(child) = &mut child_data.child else {
                continue;
            };
            if let Err(err) = update_child_policy(
                &mut child.policy,
                child_data.latest_update.clone(),
                &child_data.child_config,
                channel_controller,
            ) {
                errs.push(err);
            }
        }
        self.reconcile(channel_controller);
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs.join("; "))
        }
    }

    fn work(&mut self, data: Option<WorkData>, channel_controller: &mut dyn ChannelController) {
        debug_assert!(
            data.is_some(),
            "PriorityPolicy::work called with None value"
        );
        // Work items scheduled by PriorityPolicy's own timers only trigger
        // reconciliation; all other work items belong to a child policy.
        if let Some(mut data) = data
            && data.downcast_ref::<PriorityTimerWork>().is_none()
        {
            // Offer the item to each child until one claims it.  Items
            // belonging to deleted children are dropped.
            for child in self.children.iter_mut().filter_map(|c| c.child.as_mut()) {
                match child.policy.try_work(data, channel_controller) {
                    Ok(()) => break,
                    Err(unclaimed) => data = unclaimed,
                }
            }
        }
        self.reconcile(channel_controller);
    }

    fn exit_idle(&mut self, channel_controller: &mut dyn ChannelController) {
        // Only the currently selected child is asked to exit idle: it is the
        // one whose picker is being used.
        let Some(child) = self
            .current_priority
            .and_then(|idx| self.children[idx].child.as_mut())
        else {
            return;
        };
        child.policy.exit_idle(channel_controller);
        self.reconcile(channel_controller);
    }
}

/// Applies `child_config` and `update` to a priority child's
/// [`GracefulSwitchPolicy`].
fn update_child_policy(
    policy: &mut Child<GracefulSwitchPolicy>,
    update: ResolverUpdate,
    child_config: &PriorityChildConfig,
    channel_controller: &mut dyn ChannelController,
) -> Result<(), String> {
    policy.set_suppress_resolution(child_config.ignore_reresolution_requests);
    let gs_cfg = GracefulSwitchLbConfig::new(
        child_config.config.builder.clone(),
        child_config.config.config.clone(),
    );
    policy.resolver_update(update, &gs_cfg, channel_controller)
}

impl CreatedChild {
    /// Updates `state` to reflect the connectivity state most recently
    /// reported by the child policy and the state of its failover timer.
    /// Starts a failover timer if the child starts connecting.
    fn update_state(&mut self, work_scheduler: &Arc<dyn WorkScheduler>, rt: &GrpcRuntime) {
        self.state
            .update(self.policy.state().connectivity_state, work_scheduler, rt);
    }
}

impl PriorityPolicy {
    /// Reconciles timers, synchronizes child connectivity states, and
    /// re-evaluates priority selection.
    ///
    /// This is the central event handler invoked after resolver updates,
    /// subchannel state changes, or timer expirations. It executes three
    /// sequential steps:
    /// 1. [`handle_deactivation_timer`]: Prunes deactivated children whose
    ///    15-minute timer has expired.
    /// 2. [`update_child_data`]: Synchronizes each child's [`ChildState`]
    ///    with the state reported by its child policy and its failover timer.
    /// 3. [`choose_priority`]: Executes the idempotent priority selection
    ///    algorithm ([gRFC A56]).
    ///
    /// [gRFC A56]:
    ///   https://github.com/grpc/proposal/blob/master/A56-priority-lb-policy.md
    fn reconcile(&mut self, channel_controller: &mut dyn ChannelController) {
        self.handle_deactivation_timer();
        self.update_child_data();
        self.choose_priority(channel_controller);
    }

    /// Prunes deactivated children whose 15-minute retention timer has expired.
    ///
    /// The child policies of expired children are deleted, tearing down their
    /// subchannels. The children are NOT removed from `self.children` so they
    /// can be lazily re-created if higher priorities fail later (see [gRFC A56
    /// (Section Child Lifetime Management)]).
    fn handle_deactivation_timer(&mut self) {
        for child_data in &mut self.children {
            if let Some(CreatedChild {
                state: ChildState::Deactivated(timer),
                ..
            }) = &child_data.child
                && timer.expired()
            {
                child_data.child = None;
            }
        }
    }

    /// Synchronizes the [`ChildState`] of each child with the latest state
    /// reported by its child policy and its failover timer.
    ///
    /// Children that have not been created yet are left untouched.
    fn update_child_data(&mut self) {
        for child in self.children.iter_mut().filter_map(|c| c.child.as_mut()) {
            child.update_state(&self.work_scheduler, &self.rt);
        }
    }

    /// Evaluates the priority hierarchy and selects the active child policy to
    /// route traffic.
    ///
    /// Implements the idempotent selection algorithm defined in
    /// [gRFC A56 (Section Algorithm for Choosing a Priority)]:
    ///   https://github.com/grpc/proposal/blob/master/A56-priority-lb-policy.md#algorithm-for-choosing-a-priority
    fn choose_priority(&mut self, channel_controller: &mut dyn ChannelController) {
        // If priority list is empty, report TRANSIENT_FAILURE with
        // FailingPicker.
        if self.children.is_empty() {
            self.current_priority = None;
            self.update_picker(
                channel_controller,
                LbState {
                    connectivity_state: ConnectivityState::TransientFailure,
                    picker: Arc::new(FailingPicker {
                        error: "priority policy has empty priority list".to_owned(),
                    }),
                },
            );
            return;
        }

        // Iterate through priorities in decreasing priority order (0..N-1),
        // searching for a child in READY/IDLE or whose 10s failover timer is
        // still pending.
        for idx in 0..self.children.len() {
            let child_data = &mut self.children[idx];

            let child = match &mut child_data.child {
                Some(child) => {
                    // Reactivate child if previously deactivated.
                    if let ChildState::Deactivated(_) = child.state {
                        child.state = ChildState::from_connectivity_state(
                            child.policy.state().connectivity_state,
                            &self.work_scheduler,
                            &self.rt,
                        );
                    }
                    child
                }
                // Lazily create and initialize the child if uninitialized.
                None => {
                    let mut policy = Child::from_fn(
                        "graceful_switch",
                        LbPolicyOptions {
                            work_scheduler: self.work_scheduler.clone(),
                            runtime: self.rt.clone(),
                        },
                        |options| {
                            GracefulSwitchPolicy::new(options.runtime, options.work_scheduler)
                        },
                    );
                    if update_child_policy(
                        &mut policy,
                        child_data.latest_update.clone(),
                        &child_data.child_config,
                        channel_controller,
                    )
                    .is_err()
                    {
                        channel_controller.request_resolution();
                    }
                    let state = ChildState::from_connectivity_state(
                        policy.state().connectivity_state,
                        &self.work_scheduler,
                        &self.rt,
                    );
                    child_data.child.insert(CreatedChild { policy, state })
                }
            };

            match child.state {
                // Child is Connecting and failover timer is pending: use this
                // child, without deactivating lower priorities.
                ChildState::Connecting(_) => {
                    self.set_current_priority(channel_controller, idx, false);
                    return;
                }
                // Child failover timer expired or in transient failure: skip to
                // lower priorities.
                ChildState::ConnectingExpired | ChildState::TransientFailure => {}
                // Child is Ready or Idle: use this child and deactivate lower
                // priorities.
                ChildState::ReadyOrIdle => {
                    self.set_current_priority(channel_controller, idx, true);
                    return;
                }
                ChildState::Deactivated(_) => {
                    unreachable!("child was reactivated and cannot remain in Deactivated state")
                }
            }
        }

        // We did not find a priority in READY or IDLE or whose failover timer
        // was pending, so check for one in CONNECTING (whose failover timer has
        // expired).
        if let Some(idx) = self.children.iter().position(|c| {
            c.child
                .as_ref()
                .is_some_and(|a| matches!(a.state, ChildState::ConnectingExpired))
        }) {
            self.set_current_priority(channel_controller, idx, false);
            return;
        }

        // We didn't find a child in CONNECTING, so delegate to the last child
        // (reporting its TRANSIENT_FAILURE state and failing picker).
        self.set_current_priority(channel_controller, self.children.len() - 1, false);
    }

    /// Activates the selected priority tier and updates the channel picker.
    fn set_current_priority(
        &mut self,
        channel_controller: &mut dyn ChannelController,
        index: usize,
        deactivate_lower_priorities: bool,
    ) {
        // Deactivate lower priorities if needed.
        if deactivate_lower_priorities {
            for child_data in self.children.iter_mut().skip(index + 1) {
                let Some(child) = &mut child_data.child else {
                    continue;
                };
                if !matches!(child.state, ChildState::Deactivated(_)) {
                    child.state = ChildState::Deactivated(Timer::new(
                        DEACTIVATION_TIMEOUT,
                        self.work_scheduler.clone(),
                        self.rt.clone(),
                    ));
                }
            }
        }

        // Use this child's picker.
        self.current_priority = Some(index);
        let lb_state = self.children[index]
            .child
            .as_ref()
            .expect("cannot set priority to uninitialized child; child must be initialized first")
            .policy
            .state()
            .clone();
        self.update_picker(channel_controller, lb_state);
    }

    /// Updates the channel controller with the new [`LbState`] if it differs
    /// from the currently published state, recording it in
    /// `self.published_lb_state` for deduplication.
    fn update_picker(&mut self, channel_controller: &mut dyn ChannelController, lb_state: LbState) {
        if self.published_lb_state.as_ref() == Some(&lb_state) {
            return;
        }
        self.published_lb_state = Some(lb_state.clone());
        channel_controller.update_picker(lb_state);
    }
}

#[cfg(test)]
impl PriorityPolicy {
    /// Returns the data tracked for the child with the given name, if it is
    /// currently configured.
    fn child(&self, name: &str) -> Option<&ChildData> {
        self.children
            .iter()
            .find(|child_data| child_data.name == name)
    }
}

#[cfg(test)]
mod test;
