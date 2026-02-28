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

//! Types used to implement core gRPC functionality common to clients and
//! servers.  Note that most gRPC applications should not need these types
//! unless they are implementing custom interceptors.

use std::any::TypeId;

use bytes::Buf;
use tonic::metadata::MetadataMap;

use crate::Status;

#[allow(unused)]
pub trait SendMessage: Send + Sync {
    fn encode(&self) -> Result<Box<dyn Buf + Send + Sync>, String>;

    #[doc(hidden)]
    unsafe fn _ptr_for(&self, id: TypeId) -> Option<*const ()> {
        None
    }
}

#[allow(unused)]
pub trait RecvMessage: Send + Sync {
    fn decode(&mut self, data: &mut dyn Buf) -> Result<(), String>;

    #[doc(hidden)]
    unsafe fn _ptr_for(&mut self, id: TypeId) -> Option<*mut ()> {
        None
    }
}

/// A MessageType describes what underlying message is inside a SendMessage or
/// RecvMessage so that it can be downcast, e.g. by interceptors.  It allows for
/// safe downcasting to views containing a lifetime.
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

/// ResponseStreamItem represents an item in a response stream (either server
/// sending or client receiving).
///
/// A response stream must always contain items exactly as follows:
///
/// [Headers *Message] Trailers *StreamClosed
///
/// That is: optionaly, a Headers value and any number of Message values
/// (including zero), followed by a required Trailers value.  A response stream
/// should not be used after Trailers, but reads should return StreamClosed if
/// it is.
#[derive(Debug, Clone)]
pub enum ResponseStreamItem<M> {
    /// Indicates the headers for the stream.
    Headers(ResponseHeaders),
    /// Indicates a message on the stream.
    Message(M),
    /// Indicates trailers were received on the stream and includes the trailers.
    Trailers(Trailers),
    /// Indicates the response stream was closed.  Trailers must have been
    /// provided before this value may be used.
    StreamClosed,
}

/// The client's view of a ResponseStream in a RecvStream: the message type is
/// void as the received message is passed in via the `next` method.
pub type ClientResponseStreamItem = ResponseStreamItem<()>;

/// The server's view of a ResponseStream in a SendStream: the message type is
/// part of the payload provided to the `send` method.
pub type ServerResponseStreamItem<'a> = ResponseStreamItem<&'a dyn SendMessage>;

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
}

/// Contains all information transmitted in the response trailers of an RPC.
/// gRPC does not support request trailers.
#[derive(Debug, Clone)]
pub struct Trailers {
    status: Status,
    metadata: MetadataMap,
}

impl Trailers {
    /// Returns a default RequestHeaders instance.
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

    /// Returns a reference to the status contained in these trailers.
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
}
