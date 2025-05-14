use super::{
    super::{Connection, Endpoint},
    Connector,
};

use http::Uri;
use hyper_util::client::legacy::connect::HttpConnector;
use pin_project::pin_project;
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tokio::sync::mpsc::Receiver;
use tokio_stream::Stream;
use tower::{
    discover::{Change as TowerChange, Discover},
    Service,
};

/// A change in the service set.
#[derive(Debug, Clone)]
pub enum Change<K, V> {
    /// A new service identified by key `K` was identified.
    Insert(K, V),
    /// The service identified by key `K` disappeared.
    Remove(K),
}

/// Implements [`Discover<Service = Connection>`](Discover) for any
/// [`Discover<Service = (Connector, Endpoint)>`](Discover)
#[pin_project]
pub(crate) struct MapDiscover<D> {
    #[pin]
    discover: D,
}

impl<D> MapDiscover<D> {
    pub(crate) fn new(discover: D) -> Self {
        Self { discover }
    }
}

impl<D, C> Stream for MapDiscover<D>
where
    D: Discover<Service = (C, Endpoint)>,
    C: Service<Uri> + Send + 'static,
    C::Response: hyper::rt::Read + hyper::rt::Write + Unpin + Send,
    C::Error: Into<crate::BoxError> + Send,
    C::Future: Send,
{
    type Item = Result<TowerChange<D::Key, Connection>, D::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        match this.discover.poll_discover(cx) {
            Poll::Ready(Some(Ok(change))) => match change {
                TowerChange::Insert(k, (conn, e)) => {
                    Poll::Ready(Some(Ok(TowerChange::Insert(k, Connection::lazy(conn, e)))))
                }
                TowerChange::Remove(k) => Poll::Ready(Some(Ok(TowerChange::Remove(k)))),
            },
            Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(err))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Implements [`Discover<Service = Connection>`](Discover) for [`Receiver`]
pub(crate) struct DynamicServiceStream<K> {
    changes: Receiver<Change<K, Endpoint>>,
}

impl<K> DynamicServiceStream<K> {
    pub(crate) fn new(changes: Receiver<Change<K, Endpoint>>) -> Self {
        Self { changes }
    }
}

impl<K> Stream for DynamicServiceStream<K> {
    type Item = Result<TowerChange<K, (Connector<HttpConnector>, Endpoint)>, crate::BoxError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.changes).poll_recv(cx) {
            Poll::Pending | Poll::Ready(None) => Poll::Pending,
            Poll::Ready(Some(change)) => match change {
                Change::Insert(k, e) => {
                    Poll::Ready(Some(Ok(TowerChange::Insert(k, (e.http_connector(), e)))))
                }
                Change::Remove(k) => Poll::Ready(Some(Ok(TowerChange::Remove(k)))),
            },
        }
    }
}
