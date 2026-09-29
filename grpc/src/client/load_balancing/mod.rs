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
use std::any::Any;
use std::error::Error;
use std::fmt::Debug;
use std::fmt::Display;
use std::sync::Arc;

use crate::StatusCodeError;
use crate::StatusError;
use crate::client::ConnectivityState;
use crate::client::RequestHeaders;
use crate::client::load_balancing::subchannel::Subchannel;
use crate::client::load_balancing::subchannel::SubchannelState;
use crate::client::name_resolution::ResolverUpdate;
use crate::core::Address;
use crate::metadata::MetadataMap;
use crate::rt::GrpcRuntime;

pub(crate) mod subchannel_sharing;

pub mod child_manager;
pub mod endpoint_filtering;
pub mod graceful_switch;
pub mod lazy;
pub mod pick_first;
pub mod registry;
pub mod round_robin;
pub mod subchannel;
pub use registry::GLOBAL_LB_REGISTRY;

#[cfg(test)]
pub(crate) mod test_utils;

/// An LB policy factory that produces LbPolicy instances used by the channel
/// to manage connections and pick connections for RPCs.
pub trait LbPolicyBuilder: Send + Sync + Debug + 'static {
    type LbPolicy: LbPolicy;

    /// Builds and returns a new LB policy instance.
    ///
    /// Note that build must not fail.  Any optional configuration is delivered
    /// via the LbPolicy's resolver_update method.
    ///
    /// An LbPolicy instance is assumed to begin in a Connecting state that
    /// queues RPCs until its first update.
    fn build(&self, options: LbPolicyOptions) -> Self::LbPolicy;

    /// Reports the name of the LB Policy.
    fn name(&self) -> &'static str;

    /// Parses the JSON LB policy configuration into an internal representation.
    ///
    /// LB policies do not need to accept a configuration, in which case the
    /// default implementation returns Ok(None).
    fn parse_config(
        &self,
        _config: &LbConfigJson,
    ) -> Result<<Self::LbPolicy as LbPolicy>::LbConfig, String>;
}

/// An LB policy instance.
///
/// LB policies are responsible for creating connections (modeled as
/// Subchannels) and producing Picker instances for picking connections for
/// RPCs.
pub trait LbPolicy: Send + Sync + Debug + 'static {
    type LbConfig: Any + Send + Sync + Debug + 'static;

    /// Called by the channel when the name resolver produces a new set of
    /// resolved addresses or a new service config.
    fn resolver_update(
        &mut self,
        update: ResolverUpdate,
        config: &Self::LbConfig,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), String>;

    /// Called by the channel in response to a call from the LB policy to the
    /// WorkScheduler's `schedule_work` method.
    ///
    /// This is also how subchannel state updates are delivered: when a
    /// subchannel created by this policy changes state, the channel schedules
    /// work on the WorkScheduler that was passed to
    /// [`ChannelController::new_subchannel`], and `data` contains a
    /// [`SubchannelUpdate`](subchannel::SubchannelUpdate).
    fn work(&mut self, data: Option<WorkData>, channel_controller: &mut dyn ChannelController);

    /// Called by the channel when an LbPolicy goes idle and the channel
    /// wants it to start connecting to subchannels again.
    fn exit_idle(&mut self, channel_controller: &mut dyn ChannelController);
}

/// A collection of data configured on the channel that is constructing this
/// LbPolicy.
#[derive(Debug)]
pub struct LbPolicyOptions {
    /// A hook into the channel's work scheduler that allows the LbPolicy to
    /// request the ability to perform operations on the ChannelController.
    pub work_scheduler: Arc<dyn WorkScheduler>,
    pub runtime: GrpcRuntime,
}

/// A trait to add `Debug` to an `Any` for [`WorkData`] to allow debugging data
/// to be printed more readily.  Blanket implemented on all types that are Any +
/// Send + Debug.  `dyn WorkDataTrait` also implements downcast methods like
/// [`Any`] for convenience.
pub trait WorkDataTrait: Any + Send + Debug {}

impl<T: Any + Send + Debug> WorkDataTrait for T {}

impl dyn WorkDataTrait {
    /// Like [`Box<dyn Any>::downcast`] but for this wrapper trait.
    pub fn downcast<T: Any>(self: Box<Self>) -> Result<Box<T>, Box<Self>> {
        // If we directly call downcast then we can't return `Self` anymore
        // (only a Box<dyn Any + Send>), so we first have to check `is` and only
        // downcast when we know it will succeed.
        if (&*self as &(dyn Any + Send)).is::<T>() {
            Ok((self as Box<dyn Any + Send>).downcast().unwrap())
        } else {
            Err(self)
        }
    }

    /// Like
    /// [`downcast_ref`](https://doc.rust-lang.org/std/any/trait.Any.html#method.downcast_ref)
    /// implemented on [`dyn Any`](Any), but for this wrapper trait.
    pub fn downcast_ref<T: Any>(&self) -> Option<&T> {
        (self as &(dyn Any + Send)).downcast_ref::<T>()
    }

    /// Like
    /// [`downcast_mut`](https://doc.rust-lang.org/std/any/trait.Any.html#method.downcast_mut)
    /// implemented on [`dyn Any`](Any), but for this wrapper trait.
    pub fn downcast_mut<T: Any>(&mut self) -> Option<&mut T> {
        (self as &mut (dyn Any + Send)).downcast_mut::<T>()
    }
}

/// A dynamic payload passed between [`WorkScheduler::schedule_work`] and its
/// associated policy's [`work`](LbPolicy::work) method.
pub type WorkData = Box<dyn WorkDataTrait>;

/// Used to asynchronously request a call into the LbPolicy's work method if
/// the LbPolicy needs to provide an update without waiting for an update
/// from the channel first.
pub trait WorkScheduler: Send + Sync + Debug {
    /// Schedules a call into the LbPolicy's work method.  Multiple work calls
    /// carrying a `data` payload of `None` may be coalesced with one another.
    fn schedule_work(&self, data: Option<WorkData>);
}

/// A resolved load balancing policy builder and its parsed configuration.
#[derive(Clone, Debug)]
pub struct ParsedLbConfig {
    /// The registered builder for the selected load balancing policy.
    pub builder: Arc<DynLbPolicyBuilder>,
    /// The policy-specific configuration produced by [`LbPolicyBuilder::parse_config`].
    pub config: DynLbConfig,
}

impl ParsedLbConfig {
    /// Evaluates a non-empty gRFC A24 `LoadBalancingConfig` JSON array string
    /// against the global LB registry, selecting the first registered policy
    /// and parsing its configuration.
    pub fn parse(json: &str) -> Result<Self, String> {
        let value: serde_json::Value = serde_json::from_str(json)
            .map_err(|e| format!("failed to parse LB config JSON: {e}"))?;
        Self::from_value(value)?.ok_or_else(|| {
            "Load balancing policy configuration list must not be empty.".to_string()
        })
    }

    /// Evaluates an optional gRFC A24 `LoadBalancingConfig` JSON value against
    /// the global LB registry.
    ///
    /// Returns `Ok(None)` if the JSON value is `null` or an empty array (`[]`),
    /// allowing top-level service configuration to fall back to
    /// `loadBalancingPolicy` or the default policy.
    pub(crate) fn from_value(value: serde_json::Value) -> Result<Option<Self>, String> {
        if value.is_null() {
            return Ok(None);
        }

        let serde_json::Value::Array(entries) = value else {
            return Err("Load balancing configuration must be a JSON array.".to_string());
        };

        if entries.is_empty() {
            return Ok(None);
        }

        for entry in entries {
            let serde_json::Value::Object(map) = entry else {
                return Err("Each load balancing config entry must be a JSON object.".to_string());
            };

            let mut iter = map.into_iter();
            let (Some((name, raw_config)), None) = (iter.next(), iter.next()) else {
                return Err(
                    "Each load balancing config entry must contain exactly one policy name."
                        .to_string(),
                );
            };

            if let Some(builder) = GLOBAL_LB_REGISTRY.get_policy(&name) {
                let lb_config_json = LbConfigJson::from_value(raw_config);
                let parsed_config = builder.parse_config(&lb_config_json)?;
                return Ok(Some(ParsedLbConfig {
                    builder,
                    config: parsed_config,
                }));
            }
        }

        Err("No supported load balancing policy found in config.".to_string())
    }
}

impl<'de> serde::Deserialize<'de> for ParsedLbConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        Self::from_value(value)
            .map_err(serde::de::Error::custom)?
            .ok_or_else(|| {
                serde::de::Error::custom(
                    "Load balancing policy configuration list must not be empty.",
                )
            })
    }
}

/// Abstract representation of the configuration for any LB policy, stored as
/// JSON.  Hides internal storage details and includes a method to deserialize
/// the JSON into a concrete policy struct.
#[derive(Clone, Debug)]
pub struct LbConfigJson {
    value: serde_json::Value,
}

impl LbConfigJson {
    /// Creates a new LbConfigJson from the provided JSON string.
    pub fn new(json: &str) -> Result<Self, String> {
        match serde_json::from_str(json) {
            Ok(value) => Ok(LbConfigJson { value }),
            Err(e) => Err(format!("failed to parse LB config JSON: {e}")),
        }
    }

    /// Creates an empty JSON object configuration (`{}`).
    pub fn empty() -> Self {
        Self {
            value: serde_json::Value::Object(serde_json::Map::new()),
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

/// Controls channel behaviors.
pub trait ChannelController: Send + Sync {
    /// Creates a new subchannel and returns its current state.
    ///
    /// Whenever the subchannel changes state, the channel calls
    /// [`schedule_work`](WorkScheduler::schedule_work) on `work_scheduler`
    /// with a [`SubchannelUpdate`](subchannel::SubchannelUpdate) describing the
    /// new state.  Policies should generally pass the WorkScheduler they were
    /// given in [`LbPolicyOptions`] so the update is routed back to them.
    ///
    /// Note that `work_scheduler` may be called before this method returns.
    /// However, no update for the state returned by this method should be
    /// expected.
    fn new_subchannel(
        &mut self,
        address: &Address,
        work_scheduler: Arc<dyn WorkScheduler>,
    ) -> (Arc<dyn Subchannel>, SubchannelState);

    /// Provides a new snapshot of the LB policy's state to the channel.
    fn update_picker(&mut self, update: LbState);

    /// Signals the name resolver to attempt to re-resolve addresses.  Typically
    /// used when connections fail, indicating a possible change in the overall
    /// network configuration.
    fn request_resolution(&mut self);
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
pub trait Picker: Send + Sync + Debug {
    /// Picks a connection to use for the request.
    ///
    /// This function should not block.  If the Picker needs to do blocking or
    /// time-consuming work to service this request, it should return Queue, and
    /// the Pick call will be repeated by the channel when a new Picker is
    /// produced by the LbPolicy.
    fn pick(&self, request: &RequestHeaders) -> PickResult;
}

#[derive(Debug)]
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
    Fail(StatusError),
    /// Indicates that the request should fail with the included status
    /// immediately, even if the RPC is wait-for-ready.  The channel will
    /// convert the status code to INTERNAL if it is not a valid code for the
    /// gRPC library to produce, per [gRFC A54].
    ///
    /// [gRFC A54]:
    ///     https://github.com/grpc/proposal/blob/master/A54-restrict-control-plane-status-codes.md
    Drop(StatusError),
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
            Self::Fail(st) => write!(f, "Fail({st:?})"),
            Self::Drop(st) => write!(f, "Drop({st:?})"),
        }
    }
}

/// State provided by the LB policy to the channel.
#[derive(Clone, Debug)]
pub struct LbState {
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
pub type CompletionCallback = Box<dyn Fn() + Send + Sync>;

/// A collection of data used by the channel for routing a request.
pub struct Pick {
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

/// OneSubchannelPicker always returns a single subchannel.
#[derive(Debug)]
pub(crate) struct OneSubchannelPicker {
    sc: Arc<dyn Subchannel>,
}

impl Picker for OneSubchannelPicker {
    fn pick(&self, _: &RequestHeaders) -> PickResult {
        PickResult::Pick(Pick {
            subchannel: self.sc.clone(),
            metadata: MetadataMap::new(),
            on_complete: None,
        })
    }
}

/// QueuingPicker always returns Queue.  LB policies that are not actively
/// Connecting should not use this picker.
#[derive(Debug)]
pub(crate) struct QueuingPicker;

impl Picker for QueuingPicker {
    fn pick(&self, _request: &RequestHeaders) -> PickResult {
        PickResult::Queue
    }
}

#[derive(Debug)]
pub(crate) struct FailingPicker {
    pub error: String,
}

impl Picker for FailingPicker {
    fn pick(&self, _: &RequestHeaders) -> PickResult {
        PickResult::Fail(StatusError::new(
            StatusCodeError::Unavailable,
            self.error.clone(),
        ))
    }
}

/// A dynamic LB policy config implementation that can be downcast to a specific
/// config as needed.
pub(crate) type DynLbConfig = Arc<dyn Any + Send + Sync>;

/// A builder of dynamic LB policies.
pub(crate) type DynLbPolicyBuilder = dyn LbPolicyBuilder<LbPolicy = Box<DynLbPolicy>>;

/// An LB policy that accepts dynamic configs.
pub(crate) type DynLbPolicy = dyn LbPolicy<LbConfig = DynLbConfig>;

impl<T: LbPolicy + ?Sized> LbPolicy for Box<T> {
    type LbConfig = T::LbConfig;

    fn resolver_update(
        &mut self,
        update: ResolverUpdate,
        config: &Self::LbConfig,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), String> {
        (**self).resolver_update(update, config, channel_controller)
    }

    fn work(&mut self, data: Option<WorkData>, channel_controller: &mut dyn ChannelController) {
        (**self).work(data, channel_controller);
    }

    fn exit_idle(&mut self, channel_controller: &mut dyn ChannelController) {
        (**self).exit_idle(channel_controller);
    }
}

impl<B: LbPolicyBuilder + ?Sized> LbPolicyBuilder for Arc<B> {
    type LbPolicy = B::LbPolicy;

    fn build(&self, options: LbPolicyOptions) -> Self::LbPolicy {
        (**self).build(options)
    }

    fn name(&self) -> &'static str {
        (**self).name()
    }

    fn parse_config(
        &self,
        config: &LbConfigJson,
    ) -> Result<<B::LbPolicy as LbPolicy>::LbConfig, String> {
        (**self).parse_config(config)
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;
    use crate::client::load_balancing::pick_first::PickFirstConfig;

    #[test]
    fn parsed_lb_config_selects_first_registered_policy() {
        let resolved = ParsedLbConfig::parse(
            r#"[
                { "unsupported_policy": { "key": "value" } },
                { "pick_first": { "shuffleAddressList": true } },
                { "round_robin": {} }
            ]"#,
        )
        .unwrap();

        assert_eq!(resolved.builder.name(), "pick_first");
        assert!(
            resolved
                .config
                .as_ref()
                .downcast_ref::<PickFirstConfig>()
                .is_some()
        );

        let invalid = ParsedLbConfig::parse(
            r#"[
                { "pick_first": { "shuffleAddressList": "not_a_bool" } },
                { "round_robin": {} }
            ]"#,
        );
        assert!(invalid.is_err());
    }

    #[test]
    fn parsed_lb_config_rejects_empty_and_null() {
        assert!(ParsedLbConfig::parse("[]").is_err());
        assert!(
            ParsedLbConfig::from_value(serde_json::json!([]))
                .unwrap()
                .is_none()
        );

        assert!(ParsedLbConfig::parse("null").is_err());
        assert!(
            ParsedLbConfig::from_value(serde_json::Value::Null)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn parsed_lb_config_deserializes_in_parent_config_struct() {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct ParentConfig {
            child_policy: ParsedLbConfig,
            fallback_policy: ParsedLbConfig,
        }

        let parent_json = LbConfigJson::new(
            r#"{
                "childPolicy": [{ "round_robin": {} }],
                "fallbackPolicy": [{ "pick_first": { "shuffleAddressList": true } }]
            }"#,
        )
        .unwrap();

        let parsed: ParentConfig = parent_json.convert_to().unwrap();
        assert_eq!(parsed.child_policy.builder.name(), "round_robin");
        assert_eq!(parsed.fallback_policy.builder.name(), "pick_first");
    }
}
