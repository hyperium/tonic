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

//! Resource watcher types.

use std::marker::PhantomData;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::sync::{mpsc, oneshot};

use crate::client::worker::{WatchEvent, WatcherId, WorkerCommand};
use crate::error::Error;
use crate::resource::{DecodedResource, Resource};

/// A signal to indicate that processing of a resource event is complete.
///
/// The xDS client does not read the next response from the ADS stream until
/// every watcher has signaled for the current one (ADS flow control, per
/// gRFC A88). This lets watchers apply an update — including adding cascading
/// subscriptions (e.g. LDS -> RDS -> CDS -> EDS) — before the next update can
/// arrive. It does *not* delay the ACK/NACK, and the worker keeps processing
/// watch/unwatch commands while waiting, so holding the token cannot deadlock
/// the client.
///
/// # Automatic Signaling
///
/// Signals automatically when dropped. If you have cascading watches to add, simply
/// add them before dropping the `ProcessingDone`.
///
/// # Example
///
/// ```ignore
/// match event {
///     ResourceEvent::ResourceChanged { result: Ok(resource), done } => {
///         // Process the new resource, possibly add cascading watches.
///         client.watch::<RouteConfiguration>(&resource.route_name()).await;
///         // Signal is sent automatically when done is dropped
///     }
///     ResourceEvent::ResourceChanged { result: Err(error), done } => {
///         // Resource was invalidated (validation error or deleted)
///         eprintln!("Resource invalidated: {}", error);
///         // Stop using the previously cached resource
///     }
///     ResourceEvent::AmbientError { error, .. } => {
///         // Non-fatal error, continue using cached resource
///         eprintln!("Ambient error: {}", error);
///     }
/// }
/// ```
#[derive(Debug)]
pub struct ProcessingDone(Option<Arc<oneshot::Sender<()>>>);

impl ProcessingDone {
    /// Create the shared signal for one response.
    ///
    /// Returns a `ProcessingDone` token and a receiver future that resolves
    /// once the token and every [`share`](Self::share) of it are dropped
    /// (dropping the last `Arc` drops the sender, which wakes the receiver).
    pub(crate) fn channel() -> (Self, oneshot::Receiver<()>) {
        let (tx, rx) = oneshot::channel();
        (Self(Some(Arc::new(tx))), rx)
    }

    /// Another token tied to the same response's signal.
    ///
    /// All tokens for one response share a single allocation; the receiver
    /// resolves only after every one of them is dropped.
    pub(crate) fn share(&self) -> Self {
        Self(self.0.clone())
    }

    /// A token that signals nothing when dropped.
    ///
    /// Used internally for events that do not participate in ADS flow
    /// control, and useful for constructing [`ResourceEvent`]s in tests.
    pub fn detached() -> Self {
        Self(None)
    }
}

/// Events delivered to resource watchers.
///
/// Per gRFC A88, there are two types of events:
/// - `ResourceChanged`: Indicates a change in the resource's cached state
/// - `AmbientError`: Non-fatal errors that don't affect the cached resource
#[derive(Debug)]
pub enum ResourceEvent<T> {
    /// Indicates a change in the resource's cached state.
    ///
    /// This event is sent when:
    /// - A new valid resource is received (`Ok(resource)`)
    /// - A validation error occurred (`Err(Error::Validation(...))`)
    /// - The resource was deleted or doesn't exist (`Err(Error::ResourceDoesNotExist)`)
    ///
    /// When `result` is `Err`, the previously cached resource (if any) should be
    /// invalidated. The watcher should stop using the old resource data.
    ///
    /// The resource is wrapped in `Arc` because multiple watchers may
    /// subscribe to the same resource and share the same data.
    ResourceChanged {
        /// The result of the resource update.
        /// - `Ok(resource)`: New valid resource received
        /// - `Err(error)`: Cache-invalidating error (validation failure, does not exist)
        result: Result<Arc<T>, Error>,
        /// Signal when processing is complete.
        done: ProcessingDone,
    },
    /// Indicates a non-fatal error that doesn't affect the cached resource.
    ///
    /// This is sent for transient errors like temporary connectivity issues
    /// with the xDS management server. The previously cached resource (if any)
    /// should continue to be used.
    ///
    /// Per gRFC A88, ambient errors should not cause the client to stop using
    /// a previously valid resource.
    AmbientError {
        /// The error that occurred.
        error: Error,
        /// Signal when processing is complete.
        done: ProcessingDone,
    },
}

/// A watcher for resources of type `T`.
///
/// Call [`next()`](Self::next) to receive resource events.
/// Dropping the watcher unsubscribes from the resource.
#[derive(Debug)]
pub struct ResourceWatcher<T: Resource> {
    /// Channel to receive events from the worker.
    event_rx: mpsc::UnboundedReceiver<ResourceEvent<DecodedResource>>,
    /// Unique identifier for this watcher.
    watcher_id: WatcherId,
    /// Channel to send commands to the worker (for unwatch on drop).
    command_tx: mpsc::UnboundedSender<WorkerCommand>,
    /// Marker for the resource type.
    _marker: PhantomData<T>,
}

impl<T: Resource> ResourceWatcher<T> {
    /// Create a new resource watcher.
    pub(crate) fn new(
        event_rx: mpsc::UnboundedReceiver<ResourceEvent<DecodedResource>>,
        watcher_id: WatcherId,
        command_tx: mpsc::UnboundedSender<WorkerCommand>,
    ) -> Self {
        Self {
            event_rx,
            watcher_id,
            command_tx,
            _marker: PhantomData,
        }
    }

    /// Returns the next resource event.
    ///
    /// Returns `None` when the subscription is closed (worker shut down).
    ///
    /// # Example
    ///
    /// ```ignore
    /// while let Some(event) = watcher.next().await {
    ///     match event {
    ///         ResourceEvent::ResourceChanged { result: Ok(resource), done } => {
    ///             // Process the new resource, possibly add cascading watches.
    ///             client.watch::<RouteConfiguration>(&resource.route_name()).await;
    ///             // Signal is sent automatically when done is dropped
    ///         }
    ///         ResourceEvent::ResourceChanged { result: Err(error), done } => {
    ///             // Resource was invalidated (validation error or deleted)
    ///             eprintln!("Resource invalidated: {}", error);
    ///         }
    ///         ResourceEvent::AmbientError { error, .. } => {
    ///             // Non-fatal error, continue using cached resource
    ///             eprintln!("Ambient error: {}", error);
    ///         }
    ///     }
    /// }
    /// ```
    pub async fn next(&mut self) -> Option<ResourceEvent<T>> {
        let event = self.event_rx.recv().await?;
        Some(Self::downcast_event(event))
    }

    /// Polls for the next resource event.
    ///
    /// This is the non-async equivalent of [`next()`](Self::next), for use
    /// in manual `Future`/`Stream` implementations.
    pub fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Option<ResourceEvent<T>>> {
        self.event_rx
            .poll_recv(cx)
            .map(|opt| opt.map(Self::downcast_event))
    }

    /// Converts a raw `DecodedResource` event into a typed `ResourceEvent<T>`.
    fn downcast_event(event: ResourceEvent<DecodedResource>) -> ResourceEvent<T> {
        match event {
            ResourceEvent::ResourceChanged { result, done } => {
                let typed_result = match result {
                    Ok(resource) => match resource.downcast::<T>() {
                        Some(typed_resource) => Ok(typed_resource),
                        None => Err(Error::Validation(format!(
                            "resource type mismatch (expected: {}, actual: {})",
                            std::any::type_name::<T>(),
                            resource.type_url()
                        ))),
                    },
                    Err(e) => Err(e),
                };
                ResourceEvent::ResourceChanged {
                    result: typed_result,
                    done,
                }
            }
            ResourceEvent::AmbientError { error, done } => {
                ResourceEvent::AmbientError { error, done }
            }
        }
    }
}

impl<T: Resource> Drop for ResourceWatcher<T> {
    fn drop(&mut self) {
        // Unbounded sends cannot lose an unwatch command to a full queue.
        let _ = self
            .command_tx
            .send(WorkerCommand::Watcher(WatchEvent::Unwatch {
                watcher_id: self.watcher_id,
            }));
    }
}
