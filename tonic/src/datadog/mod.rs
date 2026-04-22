//! Datadog-internal extensions to tonic.
//!
//! This module contains hooks and services that exist specifically for
//! Datadog's infrastructure needs and are **not** part of the upstream tonic
//! API.  Nothing in this module should be used directly by service owners.

pub mod rpcteam;
