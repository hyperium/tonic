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

//! Core gRPC types common to clients and servers.
//!
//! This module provides the fundamental types used in gRPC communication, such
//! as message traits, headers, and trailers.
//!
//! Most applications should not need to use these types directly, as they are
//! typically used by generated code.  However, they may be necessary when
//! implementing custom interceptors or advanced features.
//!
//! # Key Concepts
//!
//! - **[`SendMessage`] / [`RecvMessage`]:** Traits for encoding and decoding
//!   messages.
//! - **[`RequestHeaders`]:** Represents gRPC headers sent to the server to
//!   initiate a request.
//! - **[`ResponseHeaders`] / [`Trailers`]:** Represents gRPC headers and
//!   trailers received from the server during its response.

use std::any::TypeId;

use bytes::Buf;

use crate::metadata::MetadataMap;
use crate::status::Status;

/// Represents a message sent by either a client or a server.
#[allow(unused)]
pub trait SendMessage: Send + Sync {
    /// Encodes the message (`self`) as binary data.
    fn encode(&self) -> Result<Box<dyn Buf + Send + Sync>, String>;

    #[doc(hidden)]
    unsafe fn _ptr_for(&self, id: TypeId) -> Option<*const ()> {
        None
    }
}

/// Represents a message received by either a client or a server.
#[allow(unused)]
pub trait RecvMessage: Send + Sync {
    /// Encodes `data` into `self`.
    fn decode(&mut self, data: &mut dyn Buf) -> Result<(), String>;

    #[doc(hidden)]
    unsafe fn _ptr_for(&mut self, id: TypeId) -> Option<*mut ()> {
        None
    }
}

/// Describes what underlying message is inside a [`SendMessage`] or
/// [`RecvMessage`] so that it can be downcast, e.g. by interceptors.
///
/// Allows for safe downcasting to views containing a lifetime.
pub trait MessageType {
    /// The message view's type, which may have a lifetime.
    type Target<'a>;
}

fn msg_type_id<T: MessageType>() -> TypeId
where
    T::Target<'static>: 'static,
{
    TypeId::of::<T::Target<'static>>()
}

impl dyn SendMessage + '_ {
    /// Downcasts the SendMessage to T::Target if the SendMessage contains a T.
    pub fn downcast_ref<T: MessageType>(&self) -> Option<&T::Target<'_>>
    where
        T::Target<'static>: 'static,
    {
        unsafe {
            if let Some(ptr) = self._ptr_for(msg_type_id::<T>()) {
                Some(&*(ptr as *mut T::Target<'_>))
            } else {
                None
            }
        }
    }
}

#[allow(unused)]
impl dyn RecvMessage + '_ {
    /// Downcasts the RecvMessage to T::Target if the RecvMessage contains a T.
    pub fn downcast_mut<T: MessageType>(&mut self) -> Option<&mut T::Target<'_>>
    where
        T::Target<'static>: 'static,
    {
        unsafe {
            if let Some(ptr) = self._ptr_for(msg_type_id::<T>()) {
                Some(&mut *(ptr as *mut T::Target<'_>))
            } else {
                None
            }
        }
    }
}

/// Contains all information transmitted in the response headers of an RPC.
#[derive(Debug, Clone, Default)]
pub struct ResponseHeaders {
    metadata: MetadataMap,
}

impl ResponseHeaders {
    /// Returns a default ResponseHeaders instance.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the metadata of self with `metadata`.
    pub fn with_metadata(mut self, metadata: MetadataMap) -> Self {
        self.metadata = metadata;
        self
    }

    /// Returns a reference to the metadata in these headers.
    pub fn metadata(&self) -> &MetadataMap {
        &self.metadata
    }

    /// Returns a mutable reference to the metadata in these headers.
    pub fn metadata_mut(&mut self) -> &mut MetadataMap {
        &mut self.metadata
    }
}

/// Contains all information transmitted in the request headers of an RPC.
#[derive(Debug, Clone, Default)]
pub struct RequestHeaders {
    /// The full (e.g. "/Service/Method") method name specified for the call.
    method_name: String,
    /// The application-specified metadata for the call.
    metadata: MetadataMap,
}

impl RequestHeaders {
    /// Returns a default RequestHeaders instance.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the method name of self with `method_name`.
    pub fn with_method_name(mut self, method_name: impl Into<String>) -> Self {
        self.method_name = method_name.into();
        self
    }

    /// Replaces the metadata of self with `metadata`.
    pub fn with_metadata(mut self, metadata: MetadataMap) -> Self {
        self.metadata = metadata;
        self
    }

    /// Returns the full (e.g. "/Service/Method") method name for these headers.
    pub fn method_name(&self) -> &String {
        &self.method_name
    }

    /// Returns a reference to the metadata in these headers.
    pub fn metadata(&self) -> &MetadataMap {
        &self.metadata
    }

    /// Returns a mutable reference to the metadata in these headers.
    pub fn metadata_mut(&mut self) -> &mut MetadataMap {
        &mut self.metadata
    }

    /// Returns the owned fields in the RequestHeaders.
    // TODO: make public once fields are fixed.
    pub(crate) fn into_parts(self) -> (String, MetadataMap) {
        (self.method_name, self.metadata)
    }
}

/// Contains all information transmitted in the response trailers of an RPC.
/// gRPC does not support request trailers.
#[derive(Debug, Clone)]
pub struct Trailers {
    status: Status,
    metadata: MetadataMap,
}

impl Trailers {
    /// Returns a default [`Trailers`] instance.
    pub fn new(status: Status) -> Self {
        Self {
            status,
            metadata: MetadataMap::default(),
        }
    }

    /// Replaces the status of self with `status`.
    pub fn with_status(mut self, status: Status) -> Self {
        self.status = status;
        self
    }

    /// Returns a reference to the [`Status`] contained in these trailers.
    pub fn status(&self) -> &Status {
        &self.status
    }

    /// Replaces the metadata of self with `metadata`.
    pub fn with_metadata(mut self, metadata: MetadataMap) -> Self {
        self.metadata = metadata;
        self
    }

    /// Returns a mutable reference to the metadata in these trailers.
    pub fn metadata_mut(&mut self) -> &mut MetadataMap {
        &mut self.metadata
    }

    /// Returns a reference to the metadata in these trailers.
    pub fn metadata(&self) -> &MetadataMap {
        &self.metadata
    }

    /// Returns the status in the [`Trailers`], consuming the entire status.
    pub fn into_status(self) -> Status {
        self.status
    }
}
