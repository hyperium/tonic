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

use std::any::Any;
use std::fmt::Debug;
use std::fmt::Display;
use std::hash::Hash;
use std::hash::Hasher;
use std::ptr::addr_eq;
use std::sync::Arc;
use std::sync::Weak;

use crate::client::ConnectivityState;
use crate::client::name_resolution::Address;

/// Represents the current state of a Subchannel.
#[derive(Debug, Clone)]
pub(crate) struct SubchannelState {
    /// The connectivity state of the subchannel.  See SubChannel for a
    /// description of the various states and their valid transitions.
    pub(crate) connectivity_state: ConnectivityState,
    // Set if connectivity state is TransientFailure to describe the most recent
    // connection error.  None for any other connectivity_state value.
    pub last_connection_error: Option<String>,
}

impl SubchannelState {
    pub(crate) fn idle() -> Self {
        Self {
            connectivity_state: ConnectivityState::Idle,
            last_connection_error: None,
        }
    }

    pub(crate) fn ready() -> Self {
        Self {
            connectivity_state: ConnectivityState::Ready,
            last_connection_error: None,
        }
    }

    pub(crate) fn connecting() -> Self {
        Self {
            connectivity_state: ConnectivityState::Connecting,
            last_connection_error: None,
        }
    }

    pub(crate) fn transient_failure(last_connection_error: impl Into<String>) -> Self {
        Self {
            connectivity_state: ConnectivityState::TransientFailure,
            last_connection_error: Some(last_connection_error.into()),
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

pub(crate) mod private {
    pub trait Sealed {}
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
/// - TRANSIENT_FAILURE transitions to IDLE when the reconnect backoff timer has
///   expired.  This timer scales exponentially and is reset when the subchannel
///   becomes READY.
///
/// When a Subchannel is dropped, it is disconnected automatically, and no
/// subsequent state updates will be provided for it to the LB policy.
pub(crate) trait Subchannel:
    private::Sealed + DynHash + DynPartialEq + Any + Send + Sync
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
pub(crate) struct WeakSubchannel(Weak<dyn Subchannel>);

impl From<&Arc<dyn Subchannel>> for WeakSubchannel {
    fn from(subchannel: &Arc<dyn Subchannel>) -> Self {
        WeakSubchannel(Arc::downgrade(subchannel))
    }
}

impl WeakSubchannel {
    pub fn new(subchannel: &Arc<dyn Subchannel>) -> Self {
        WeakSubchannel(Arc::downgrade(subchannel))
    }

    pub fn upgrade(&self) -> Option<Arc<dyn Subchannel>> {
        self.0.upgrade()
    }

    pub fn strong_count(&self) -> usize {
        self.0.strong_count()
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

pub(crate) trait ForwardingSubchannel: DynHash + DynPartialEq + Any + Send + Sync {
    fn delegate(&self) -> &Arc<dyn Subchannel>;

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
impl<T: ForwardingSubchannel> private::Sealed for T {}
