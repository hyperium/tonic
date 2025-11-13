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

use core::panic;
use std::{
    any::Any,
    error::Error,
    fmt::{Debug, Display},
    hash::{Hash, Hasher},
    ptr::addr_eq,
    sync::{Arc, Mutex, Weak},
};
use tonic::{metadata::MetadataMap, Status};

use crate::{
    client::channel::WorkQueueTx,
    rt::Runtime,
    service::{Request, Response},
};

use crate::client::{
    channel::{InternalChannelController, WorkQueueItem},
    name_resolution::{Address, ResolverUpdate},
    subchannel::InternalSubchannel,
    ConnectivityState,
};

pub(crate) mod child_manager;
pub(crate) mod graceful_switch;
pub(crate) mod pick_first;

#[cfg(test)]
pub(crate) mod test_utils;

pub(crate) mod registry;
use super::{service_config::LbConfig, subchannel::SubchannelStateWatcher};
pub(crate) use registry::GLOBAL_LB_REGISTRY;

/// A collection of data configured on the channel that is constructing this
/// LbPolicy.
#[derive(Debug)]
pub(crate) struct LbPolicyOptions {
    /// A hook into the channel's work scheduler that allows the LbPolicy to
    /// request the ability to perform operations on the ChannelController.
    pub work_scheduler: Arc<dyn WorkScheduler>,
    pub runtime: Arc<dyn Runtime>,
}

/// Used to asynchronously request a call into the LbPolicy's work method if
/// the LbPolicy needs to provide an update without waiting for an update
/// from the channel first.
pub(crate) trait WorkScheduler: Send + Sync + Debug {
    // Schedules a call into the LbPolicy's work method.  If there is already a
    // pending work call that has not yet started, this may not schedule another
    // call.
    fn schedule_work(&self);
}

/// Abstract representation of the configuration for any LB policy, stored as
/// JSON.  Hides internal storage details and includes a method to deserialize
/// the JSON into a concrete policy struct.
#[derive(Debug)]
pub(crate) struct ParsedJsonLbConfig {
    value: serde_json::Value,
}

impl ParsedJsonLbConfig {
    /// Creates a new ParsedJsonLbConfig from the provided JSON string.
    pub fn new(json: &str) -> Result<Self, String> {
        match serde_json::from_str(json) {
            Ok(value) => Ok(ParsedJsonLbConfig { value }),
            Err(e) => Err(format!("failed to parse LB config JSON: {e}")),
        }
    }

    pub(crate) fn from_value(value: serde_json::Value) -> Self {
        Self { value }
    }

    /// Converts the JSON configuration into a concrete type that represents the
    /// configuration of an LB policy.
    ///
    /// This will typically be used by the LB policy builder to parse the
    /// configuration into a type that can be used by the LB policy.
    pub fn convert_to<T: serde::de::DeserializeOwned>(
        &self,
    ) -> Result<T, Box<dyn Error + Send + Sync>> {
        let res: T = match serde_json::from_value(self.value.clone()) {
            Ok(v) => v,
            Err(e) => {
                return Err(format!("{e}").into());
            }
        };
        Ok(res)
    }
}

/// An LB policy factory that produces LbPolicy instances used by the channel
/// to manage connections and pick connections for RPCs.
pub(crate) trait LbPolicyBuilder: Send + Sync + Debug {
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
        _config: &ParsedJsonLbConfig,
    ) -> Result<Option<LbConfig>, Box<dyn Error + Send + Sync>> {
        Ok(None)
    }
}

/// An LB policy instance.
///
/// LB policies are responsible for creating connections (modeled as
/// Subchannels) and producing Picker instances for picking connections for
/// RPCs.
pub(crate) trait LbPolicy: Send + Debug {
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
        subchannel: Arc<dyn Subchannel>,
        state: &SubchannelState,
        channel_controller: &mut dyn ChannelController,
    );

    /// Called by the channel in response to a call from the LB policy to the
    /// WorkScheduler's request_work method.
    fn work(&mut self, channel_controller: &mut dyn ChannelController);

    /// Called by the channel when an LbPolicy goes idle and the channel
    /// wants it to start connecting to subchannels again.
    fn exit_idle(&mut self, channel_controller: &mut dyn ChannelController);
}

/// Controls channel behaviors.
pub(crate) trait ChannelController: Send + Sync {
    /// Creates a new subchannel in IDLE state.
    fn new_subchannel(&mut self, address: &Address) -> Arc<dyn Subchannel>;

    /// Provides a new snapshot of the LB policy's state to the channel.
    fn update_picker(&mut self, update: LbState);

    /// Signals the name resolver to attempt to re-resolve addresses.  Typically
    /// used when connections fail, indicating a possible change in the overall
    /// network configuration.
    fn request_resolution(&mut self);
}

/// Represents the current state of a Subchannel.
#[derive(Debug, Clone)]
pub(crate) struct SubchannelState {
    /// The connectivity state of the subchannel.  See SubChannel for a
    /// description of the various states and their valid transitions.
    pub connectivity_state: ConnectivityState,
    // Set if connectivity state is TransientFailure to describe the most recent
    // connection error.  None for any other connectivity_state value.
    pub last_connection_error: Option<Arc<dyn Error + Send + Sync>>,
}

impl Default for SubchannelState {
    fn default() -> Self {
        Self {
            connectivity_state: ConnectivityState::Idle,
            last_connection_error: None,
        }
    }
}

impl Display for SubchannelState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "connectivity_state: {}", self.connectivity_state)?;
        if let Some(err) = &self.last_connection_error {
            write!(f, ", last_connection_error: {err}")?;
        }
        Ok(())
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
pub(crate) trait Picker: Send + Sync + Debug {
    /// Picks a connection to use for the request.
    ///
    /// This function should not block.  If the Picker needs to do blocking or
    /// time-consuming work to service this request, it should return Queue, and
    /// the Pick call will be repeated by the channel when a new Picker is
    /// produced by the LbPolicy.
    fn pick(&self, request: &Request) -> PickResult;
}

#[derive(Debug)]
pub(crate) enum PickResult {
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

impl PickResult {
    pub fn unwrap_pick(self) -> Pick {
        let PickResult::Pick(pick) = self else {
            panic!("Called `PickResult::unwrap_pick` on a `Queue` or `Err` value");
        };
        pick
    }
}

impl PartialEq for PickResult {
    fn eq(&self, other: &Self) -> bool {
        match self {
            PickResult::Pick(pick) => match other {
                PickResult::Pick(other_pick) => pick.subchannel == other_pick.subchannel.clone(),
                _ => false,
            },
            PickResult::Queue => matches!(other, PickResult::Queue),
            PickResult::Fail(status) => {
                // TODO: implement me.
                false
            }
            PickResult::Drop(status) => {
                // TODO: implement me.
                false
            }
        }
    }
}

impl Display for PickResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pick(_) => write!(f, "Pick"),
            Self::Queue => write!(f, "Queue"),
            Self::Fail(st) => write!(f, "Fail({st})"),
            Self::Drop(st) => write!(f, "Drop({st})"),
        }
    }
}
/// Data provided by the LB policy.
#[derive(Clone, Debug)]
pub(crate) struct LbState {
    pub connectivity_state: super::ConnectivityState,
    pub picker: Arc<dyn Picker>,
}

impl PartialEq for LbState {
    /// Equality for two LbStates.
    ///
    /// Two `LbState`s are equal if and only if they have the same connectivity
    /// state and the same Picker allocation.  Even if two Pickers have the same
    /// behavior or the same underlying implementation, they will be considered
    /// distinct unless they are the same Picker instance.
    fn eq(&self, other: &Self) -> bool {
        self.connectivity_state == other.connectivity_state
            && std::ptr::addr_eq(Arc::as_ptr(&self.picker), Arc::as_ptr(&other.picker))
    }
}

impl Eq for LbState {}

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

/// Type alias for the completion callback function.
pub(crate) type CompletionCallback = Box<dyn Fn(&Response) + Send + Sync>;

/// A collection of data used by the channel for routing a request.
pub(crate) struct Pick {
    /// The Subchannel for the request.
    pub subchannel: Arc<dyn Subchannel>,
    // Metadata to be added to existing outgoing metadata.
    pub metadata: MetadataMap,
    // Callback to be invoked once the RPC completes.
    pub on_complete: Option<CompletionCallback>,
}

impl Debug for Pick {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pick")
            .field("subchannel", &self.subchannel)
            .field("metadata", &self.metadata)
            .field("on_complete", &format_args!("{:p}", &self.on_complete))
            .finish()
    }
}

pub(crate) trait DynHash {
    #[allow(clippy::redundant_allocation)]
    fn dyn_hash(&self, state: &mut Box<&mut dyn Hasher>);
}

impl<T: Hash> DynHash for T {
    fn dyn_hash(&self, state: &mut Box<&mut dyn Hasher>) {
        self.hash(state);
    }
}

pub(crate) trait DynPartialEq {
    fn dyn_eq(&self, other: &&dyn Any) -> bool;
}

impl<T: Eq + PartialEq + 'static> DynPartialEq for T {
    fn dyn_eq(&self, other: &&dyn Any) -> bool {
        let Some(other) = other.downcast_ref::<T>() else {
            return false;
        };
        self.eq(other)
    }
}

mod private {
    pub trait Sealed {}
}

pub(crate) trait SealedSubchannel: private::Sealed {}

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
/// - TRANSIENT_FAILURE transitions to IDLE when the reconnect backoff timer has
///   expired.  This timer scales exponentially and is reset when the subchannel
///   becomes READY.
///
/// When a Subchannel is dropped, it is disconnected automatically, and no
/// subsequent state updates will be provided for it to the LB policy.
pub(crate) trait Subchannel:
    SealedSubchannel + DynHash + DynPartialEq + Any + Send + Sync
{
    /// Returns the address of the Subchannel.
    /// TODO: Consider whether this should really be public.
    fn address(&self) -> Address;

    /// Notifies the Subchannel to connect.
    fn connect(&self);
}

impl dyn Subchannel {
    pub fn downcast_ref<T>(&self) -> Option<&T>
    where
        T: 'static,
    {
        (self as &dyn Any).downcast_ref()
    }
}

impl Hash for dyn Subchannel {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.dyn_hash(&mut Box::new(state as &mut dyn Hasher));
    }
}

impl PartialEq for dyn Subchannel {
    fn eq(&self, other: &Self) -> bool {
        self.dyn_eq(&Box::new(other as &dyn Any))
    }
}

impl Eq for dyn Subchannel {}

impl Debug for dyn Subchannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Subchannel: {}", self.address())
    }
}

impl Display for dyn Subchannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Subchannel: {}", self.address())
    }
}

#[derive(Debug)]
struct WeakSubchannel(Weak<dyn Subchannel>);

impl From<Arc<dyn Subchannel>> for WeakSubchannel {
    fn from(subchannel: Arc<dyn Subchannel>) -> Self {
        WeakSubchannel(Arc::downgrade(&subchannel))
    }
}

impl WeakSubchannel {
    pub fn new(subchannel: &Arc<dyn Subchannel>) -> Self {
        WeakSubchannel(Arc::downgrade(subchannel))
    }

    pub fn upgrade(&self) -> Option<Arc<dyn Subchannel>> {
        self.0.upgrade()
    }
}

impl Hash for WeakSubchannel {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self.0.as_ptr() as *const () as usize).hash(state);
    }
}

impl PartialEq for WeakSubchannel {
    fn eq(&self, other: &Self) -> bool {
        addr_eq(self.0.as_ptr(), other.0.as_ptr())
    }
}

impl Eq for WeakSubchannel {}

pub(crate) struct ExternalSubchannel {
    pub(crate) isc: Option<Arc<InternalSubchannel>>,
    work_scheduler: WorkQueueTx,
    watcher: Mutex<Option<Arc<SubchannelStateWatcher>>>,
}

impl ExternalSubchannel {
    pub(super) fn new(isc: Arc<InternalSubchannel>, work_scheduler: WorkQueueTx) -> Self {
        ExternalSubchannel {
            isc: Some(isc),
            work_scheduler,
            watcher: Mutex::default(),
        }
    }

    pub(super) fn set_watcher(&self, watcher: Arc<SubchannelStateWatcher>) {
        self.watcher.lock().unwrap().replace(watcher);
    }
}

impl Hash for ExternalSubchannel {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.address().hash(state);
    }
}

impl PartialEq for ExternalSubchannel {
    fn eq(&self, other: &Self) -> bool {
        self.address() == other.address()
    }
}

impl Eq for ExternalSubchannel {}

impl Subchannel for ExternalSubchannel {
    fn address(&self) -> Address {
        self.isc.as_ref().unwrap().address()
    }

    fn connect(&self) {
        println!("connect called for subchannel: {self}");
        self.isc.as_ref().unwrap().connect(false);
    }
}

impl SealedSubchannel for ExternalSubchannel {}
impl private::Sealed for ExternalSubchannel {}

impl Drop for ExternalSubchannel {
    fn drop(&mut self) {
        let watcher = self.watcher.lock().unwrap().take();
        let address = self.address().address.clone();
        let isc = self.isc.take();
        let _ = self.work_scheduler.send(WorkQueueItem::Closure(Box::new(
            move |c: &mut InternalChannelController| {
                println!("unregistering connectivity state watcher for {address:?}");
                isc.as_ref()
                    .unwrap()
                    .unregister_connectivity_state_watcher(watcher.unwrap());
            },
            // The internal subchannel is dropped from here (i.e., from inside
            // the work serializer), if this is the last reference to it.
        )));
    }
}

impl Debug for ExternalSubchannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Subchannel {}", self.address())
    }
}

impl Display for ExternalSubchannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Subchannel {}", self.address())
    }
}

pub(crate) trait ForwardingSubchannel: DynHash + DynPartialEq + Any + Send + Sync {
    fn delegate(&self) -> Arc<dyn Subchannel>;

    fn address(&self) -> Address {
        self.delegate().address()
    }
    fn connect(&self) {
        self.delegate().connect()
    }
}

impl<T: ForwardingSubchannel> Subchannel for T {
    fn address(&self) -> Address {
        self.address()
    }
    fn connect(&self) {
        self.connect()
    }
}
impl<T: ForwardingSubchannel> SealedSubchannel for T {}
impl<T: ForwardingSubchannel> private::Sealed for T {}

/// QueuingPicker always returns Queue.  LB policies that are not actively
/// Connecting should not use this picker.
#[derive(Debug)]
pub(crate) struct QueuingPicker {}

impl Picker for QueuingPicker {
    fn pick(&self, _request: &Request) -> PickResult {
        PickResult::Queue
    }
}

#[derive(Debug)]
pub(crate) struct Failing {
    pub error: String,
}

impl Picker for Failing {
    fn pick(&self, _: &Request) -> PickResult {
        PickResult::Fail(Status::unavailable(self.error.clone()))
    }
}
