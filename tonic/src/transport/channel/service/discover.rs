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

use super::super::{Connection, Endpoint};

use std::{
    hash::Hash,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_stream::Stream;
use tower::discover::Change as TowerChange;

/// A change in the service set.
#[derive(Debug, Clone)]
pub enum Change<K, V> {
    /// A new service identified by key `K` was identified.
    Insert(K, V),
    /// The service identified by key `K` disappeared.
    Remove(K),
}

pub(crate) struct DynamicServiceStream<K: Hash + Eq + Clone> {
    changes: Receiver<Change<K, Endpoint>>,
    /// A sender back into the same channel `changes` reads from, handed to each
    /// discovered [`Connection`] so it can re-insert itself after being evicted by
    /// `Balance` for a connect failure. See [`Connection::lazy_for_discovery`].
    reinsert: Sender<Change<K, Endpoint>>,
}

impl<K: Hash + Eq + Clone> DynamicServiceStream<K> {
    pub(crate) fn new(
        changes: Receiver<Change<K, Endpoint>>,
        reinsert: Sender<Change<K, Endpoint>>,
    ) -> Self {
        Self { changes, reinsert }
    }
}

impl<K: Hash + Eq + Clone + Send + 'static> Stream for DynamicServiceStream<K> {
    type Item = Result<TowerChange<K, Connection>, crate::BoxError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.changes).poll_recv(cx) {
            Poll::Pending | Poll::Ready(None) => Poll::Pending,
            Poll::Ready(Some(change)) => match change {
                Change::Insert(k, endpoint) => {
                    let connection = Connection::lazy_for_discovery(
                        endpoint.http_connector(),
                        endpoint,
                        k.clone(),
                        self.reinsert.clone(),
                    );
                    Poll::Ready(Some(Ok(TowerChange::Insert(k, connection))))
                }
                Change::Remove(k) => Poll::Ready(Some(Ok(TowerChange::Remove(k)))),
            },
        }
    }
}

impl<K: Hash + Eq + Clone> Unpin for DynamicServiceStream<K> {}
