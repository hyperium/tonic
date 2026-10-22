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

//! Point-in-time views of the client's resource cache.
//!
//! [`XdsClient::resource_snapshot`](crate::XdsClient::resource_snapshot)
//! returns one [`ResourceSnapshot`] per subscribed resource. It carries what an
//! xDS config dump needs, such as a CSDS service (gRFC A40): the resource's
//! status, the last version the client accepted as received from the server,
//! and the most recent update the client rejected.

use std::time::SystemTime;

use crate::message::ResourceAny;

/// The status of a resource in the client's cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResourceStatus {
    /// The client has subscribed to the resource but has not received it.
    Requested,
    /// The server does not have the resource: it was removed from a
    /// state-of-the-world response, or it did not arrive before the resource
    /// timer fired.
    DoesNotExist,
    /// The most recent version of the resource was accepted.
    Acked,
    /// The most recent version of the resource was rejected.
    Nacked,
}

/// The most recent version of a resource that the client accepted.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AcceptedResource {
    /// The `version_info` of the response that carried the resource.
    pub version_info: String,
    /// The resource exactly as the server sent it.
    pub resource: ResourceAny,
    /// When the client accepted the resource.
    pub last_updated: SystemTime,
}

impl AcceptedResource {
    /// Creates a record of an accepted resource.
    pub fn new(
        version_info: impl Into<String>,
        resource: ResourceAny,
        last_updated: SystemTime,
    ) -> Self {
        Self {
            version_info: version_info.into(),
            resource,
            last_updated,
        }
    }
}

/// The most recent update to a resource that the client rejected.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RejectedUpdate {
    /// The `version_info` of the response that carried the rejected resource.
    pub version_info: String,
    /// Why this resource was rejected. The NACK sent to the server combines
    /// the errors of every invalid resource in the response.
    pub details: String,
    /// When the client rejected the update.
    pub last_update_attempt: SystemTime,
}

impl RejectedUpdate {
    /// Creates a record of a rejected update.
    pub fn new(
        version_info: impl Into<String>,
        details: impl Into<String>,
        last_update_attempt: SystemTime,
    ) -> Self {
        Self {
            version_info: version_info.into(),
            details: details.into(),
            last_update_attempt,
        }
    }
}

/// A point-in-time view of one subscribed resource.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ResourceSnapshot {
    /// The resource type URL.
    pub type_url: String,
    /// The resource name.
    pub name: String,
    /// The resource's status.
    pub status: ResourceStatus,
    /// The last accepted version. It is kept after the resource is rejected or
    /// removed, because a watcher may still be using it.
    pub accepted: Option<AcceptedResource>,
    /// The most recent rejected update. Set only when `status` is
    /// [`ResourceStatus::Nacked`].
    pub rejected: Option<RejectedUpdate>,
}

impl ResourceSnapshot {
    /// Creates a snapshot with no accepted version and no rejected update.
    pub fn new(
        type_url: impl Into<String>,
        name: impl Into<String>,
        status: ResourceStatus,
    ) -> Self {
        Self {
            type_url: type_url.into(),
            name: name.into(),
            status,
            accepted: None,
            rejected: None,
        }
    }

    /// Sets the last accepted version.
    pub fn with_accepted(mut self, accepted: AcceptedResource) -> Self {
        self.accepted = Some(accepted);
        self
    }

    /// Sets the most recent rejected update.
    pub fn with_rejected(mut self, rejected: RejectedUpdate) -> Self {
        self.rejected = Some(rejected);
        self
    }
}
