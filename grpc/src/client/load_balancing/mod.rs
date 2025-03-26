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

pub mod child_manager;

use std::{any::Any, error::Error, hash::Hash, sync::Arc};

use tonic::{metadata::MetadataMap, Status};

use crate::client::{
    name_resolution::{Address, ResolverUpdate},
    service::Request,
    ConnectivityState,
};

/// A collection of data configured on the channel that is constructing this
/// LbPolicy.
pub struct LbPolicyOptions {
    /// A hook into the channel's work scheduler that allows the LbPolicy to
    /// request the ability to perform operations on the ChannelController.
    pub work_scheduler: Arc<dyn WorkScheduler>,
}

/// Used to asynchronously request a call into the LbPolicy's work method if
/// the LbPolicy needs to provide an update without waiting for an update
/// from the channel first.
pub trait WorkScheduler: Send + Sync {
    // Schedules a call into the LbPolicy's work method.  If there is already a
    // pending work call that has not yet started, this may not schedule another
    // call.
    fn schedule_work(&self);
}

/// An LB policy factory that produces LbPolicy instances used by the channel
/// to manage connections and pick connections for RPCs.
pub trait LbPolicyBuilder: Send + Sync {
    /// Builds and returns a new LB policy instance.
    ///
    /// Note that build must not fail.  Any optional configuration is delivered
    /// via the LbPolicy's resolver_update method.
    ///
    /// An LbPolicy instance is assumed to begin in a Connecting state that
    /// queues RPCs until its first update.
    fn build(&self, options: LbPolicyOptions) -> Box<dyn LbPolicy>;

    /// Reports the name of the LB Policy.
    fn name(&self) -> &'static str;

    /// Parses the JSON LB policy configuration into an internal representation.
    ///
    /// LB policies do not need to accept a configuration, in which case the
    /// default implementation returns Ok(None).
    fn parse_config(
        &self,
        _config: &str,
    ) -> Result<Option<LbConfig>, Box<dyn Error + Send + Sync>> {
        Ok(None)
    }
}

/// An LB policy instance.
///
/// LB policies are responsible for creating connections (modeled as
/// Subchannels) and producing Picker instances for picking connections for
/// RPCs.
pub trait LbPolicy: Send {
    /// Called by the channel when the name resolver produces a new set of
    /// resolved addresses or a new service config.
    fn resolver_update(
        &mut self,
        update: ResolverUpdate,
        config: Option<&LbConfig>,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), Box<dyn Error + Send + Sync>>;

    /// Called by the channel when any subchannel created by the LB policy
    /// changes state.
    fn subchannel_update(
        &mut self,
        subchannel: &Subchannel,
        state: &SubchannelState,
        channel_controller: &mut dyn ChannelController,
    );

    /// Called by the channel in response to a call from the LB policy to the
    /// WorkScheduler's request_work method.
    fn work(&mut self, channel_controller: &mut dyn ChannelController);
}

/// Controls channel behaviors.
pub trait ChannelController: Send + Sync {
    /// Creates a new subchannel in IDLE state.
    fn new_subchannel(&mut self, address: &Address) -> Subchannel;

    /// Provides a new snapshot of the LB policy's state to the channel.
    fn update_picker(&mut self, update: LbState);

    /// Signals the name resolver to attempt to re-resolve addresses.  Typically
    /// used when connections fail, indicating a possible change in the overall
    /// network configuration.
    fn request_resolution(&mut self);
}

/// Represents the current state of a Subchannel.
#[derive(Clone)]
pub struct SubchannelState {
    /// The connectivity state of the subchannel.  See SubChannel for a
    /// description of the various states and their valid transitions.
    pub connectivity_state: ConnectivityState,
    // Set if connectivity state is TransientFailure to describe the most recent
    // connection error.  None for any other connectivity_state value.
    pub last_connection_error: Option<Arc<dyn Error + Send + Sync>>,
}

/// A convenience wrapper for an LB policy's configuration object.
pub struct LbConfig {
    config: Box<dyn Any>,
}

impl<'a> LbConfig {
    /// Create a new LbConfig wrapper containing the provided config.
    pub fn new(config: Box<dyn Any>) -> Self {
        LbConfig { config }
    }

    /// Converts the wrapped configuration into the type used by the LbPolicy.
    pub fn into<T: 'static>(&self) -> Option<&T> {
        self.config.downcast_ref::<T>()
    }
}

/// A Picker is responsible for deciding what Subchannel to use for any given
/// request.  A Picker is only used once for any RPC.  If pick() returns Queue,
/// the channel will queue the RPC until a new Picker is produced by the
/// LbPolicy, and will call pick() on the new Picker for the request.
///
/// Pickers are always paired with a ConnectivityState which the channel will
/// expose to applications so they can predict what might happens when
/// performing RPCs:
///
/// If the ConnectivityState is Idle, the Picker should ensure connections are
/// initiated by the LbPolicy that produced the Picker, and return a Queue
/// result so the request is attempted the next time a Picker is produced.
///
/// If the ConnectivityState is Connecting, the Picker should return a Queue
/// result and continue to wait for pending connections.
///
/// If the ConnectivityState is Ready, the Picker should return a Ready
/// Subchannel.
///
/// If the ConnectivityState is TransientFailure, the Picker should return an
/// Err with an error that describes why connections are failing.
pub trait Picker: Send + Sync {
    /// Picks a connection to use for the request.
    ///
    /// This function should not block.  If the Picker needs to do blocking or
    /// time-consuming work to service this request, it should return Queue, and
    /// the Pick call will be repeated by the channel when a new Picker is
    /// produced by the LbPolicy.
    fn pick(&self, request: &Request) -> PickResult;
}

pub enum PickResult {
    /// Indicates the Subchannel in the Pick should be used for the request.
    Pick(Pick),
    /// Indicates the LbPolicy is attempting to connect to a server to use for
    /// the request.
    Queue,
    /// Indicates that the request should fail with the included error status
    /// (with the code converted to UNAVAILABLE).  If the RPC is wait-for-ready,
    /// then it will not be terminated, but instead attempted on a new picker if
    /// one is produced before it is cancelled.
    Fail(Status),
    /// Indicates that the request should fail with the included status
    /// immediately, even if the RPC is wait-for-ready.  The channel will
    /// convert the status code to INTERNAL if it is not a valid code for the
    /// gRPC library to produce, per [gRFC A54].
    ///
    /// [gRFC A54]:
    ///     https://github.com/grpc/proposal/blob/master/A54-restrict-control-plane-status-codes.md
    Drop(Status),
}

/// Data provided by the LB policy.
#[derive(Clone)]
pub struct LbState {
    pub connectivity_state: super::ConnectivityState,
    pub picker: Arc<dyn Picker>,
}

impl LbState {
    /// Returns a generic initial LbState which is Connecting and a picker which
    /// queues all picks.
    pub fn initial() -> Self {
        Self {
            connectivity_state: ConnectivityState::Connecting,
            picker: Arc::new(QueuingPicker {}),
        }
    }
}

/// A collection of data used by the channel for routing a request.
pub struct Pick {
    /// The Subchannel for the request.
    pub subchannel: Subchannel,
    // Metadata to be added to existing outgoing metadata.
    pub metadata: MetadataMap,
}

/// A Subchannel represents a method of communicating with a server which may be
/// connected or disconnected many times across its lifetime.
///
/// - Subchannels start IDLE.
///
/// - IDLE transitions to CONNECTING when connect() is called.
///
/// - CONNECTING transitions to READY on success or TRANSIENT_FAILURE on error.
///
/// - READY transitions to IDLE when the connection is lost.
///
/// - TRANSIENT_FAILURE transitions to CONNECTING when the reconnect backoff
///   timer has expired.  This timer scales exponentially and is reset when the
///   subchannel becomes READY.
///
/// When a Subchannel is dropped, it is disconnected, and no subsequent state
/// updates will be provided for it to the LB policy.
#[derive(Clone, Debug)]
pub struct Subchannel;

impl Hash for Subchannel {
    fn hash<H: std::hash::Hasher>(&self, _state: &mut H) {
        todo!()
    }
}

impl PartialEq for Subchannel {
    fn eq(&self, _other: &Self) -> bool {
        todo!()
    }
}

impl Eq for Subchannel {}

/// QueuingPicker always returns Queue.  LB policies that are not actively
/// Connecting should not use this picker.
pub struct QueuingPicker {}

impl Picker for QueuingPicker {
    fn pick(&self, _request: &Request) -> PickResult {
        PickResult::Queue
    }
}
