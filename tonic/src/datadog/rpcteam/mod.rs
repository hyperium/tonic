//! Hooks and types intended exclusively for use by Datadog's RPC team.
//!
//! The items in this module are **not part of the public API contract** of
//! `tonic`. They exist to allow Datadog's managed-retry library to wire
//! custom retry logic into the transport layer without exposing that
//! complexity to service owners.

pub mod managed_retry_hooks;

pub use managed_retry_hooks::{
    admin_only_reset_hooks, admin_only_set_custom_retry_hook,
    admin_only_set_custom_retry_throttler, new_retry_throttler, try_custom_retry, RetryDecision,
    RetryPolicy, RetryThrottler,
};
