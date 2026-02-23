use super::super::{service::Connector, Connection, Endpoint};

use http::Uri;
use hyper::rt;
use hyper_util::client::legacy::connect::{dns::GaiResolver, HttpConnector};
use std::{
    hash::Hash,
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::sync::mpsc::Receiver;
use tokio_stream::Stream;
use tower::discover::Change as TowerChange;
use tower_service::Service;

/// A change in the service set.
#[derive(Debug, Clone)]
pub enum Change<K, V> {
    /// A new service identified by key `K` was identified.
    Insert(K, V),
    /// The service identified by key `K` disappeared.
    Remove(K),
}

/// Convert an `Endpoint` into a `(Connector<HttpConnector<GaiResolver>>, Endpoint)`.
///
/// This is needed so we can support both a stream of just `Endpoints` and a stream of `(Connector, Endpoint)` pairs.
/// We default to http connector when just `Endpoint` is provided.
impl From<Endpoint> for (Connector<HttpConnector<GaiResolver>>, Endpoint) {
    fn from(endpoint: Endpoint) -> Self {
        (endpoint.http_connector(), endpoint)
    }
}

pub(crate) struct DynamicServiceStream<K: Hash + Eq + Clone, V, C>
where
    (C, Endpoint): From<V>,
    C: Service<Uri> + Send + 'static,
    C::Error: Into<crate::BoxError> + Send,
    C::Future: Send,
    C::Response: rt::Read + rt::Write + Unpin + Send + 'static,
{
    changes: Receiver<Change<K, V>>,
    _marker: PhantomData<C>,
}

impl<K: Hash + Eq + Clone, V, C> DynamicServiceStream<K, V, C>
where
    (C, Endpoint): From<V>,
    C: Service<Uri> + Send + 'static,
    C::Error: Into<crate::BoxError> + Send,
    C::Future: Send,
    C::Response: rt::Read + rt::Write + Unpin + Send + 'static,
{
    pub(crate) fn new(changes: Receiver<Change<K, V>>) -> Self {
        Self {
            changes,
            _marker: PhantomData,
        }
    }
}

impl<K: Hash + Eq + Clone, V, C> Stream for DynamicServiceStream<K, V, C>
where
    (C, Endpoint): From<V>,
    C: Service<Uri> + Send + 'static,
    C::Error: Into<crate::BoxError> + Send,
    C::Future: Send,
    C::Response: rt::Read + rt::Write + Unpin + Send + 'static,
{
    type Item = Result<TowerChange<K, Connection>, crate::BoxError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.changes).poll_recv(cx) {
            Poll::Pending | Poll::Ready(None) => Poll::Pending,
            Poll::Ready(Some(change)) => match change {
                Change::Insert(k, connection) => {
                    let (connector, endpoint) = connection.into();
                    Poll::Ready(Some(Ok(TowerChange::Insert(
                        k,
                        Connection::lazy(connector, endpoint),
                    ))))
                }
                Change::Remove(k) => Poll::Ready(Some(Ok(TowerChange::Remove(k)))),
            },
        }
    }
}

impl<K: Hash + Eq + Clone, V, C> Unpin for DynamicServiceStream<K, V, C>
where
    (C, Endpoint): From<V>,
    C: Service<Uri> + Send + 'static,
    C::Error: Into<crate::BoxError> + Send,
    C::Future: Send,
    C::Response: rt::Read + rt::Write + Unpin + Send + 'static,
{
}
