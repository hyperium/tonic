use futures::Stream;
use std::pin::Pin;
use std::task::{Context, Poll};

// A dummy Stream implementation for examples that require
// a Stream type but don't actually use it.
pub struct ResponseStream<T>(T);

impl<T: Unpin> Stream for ResponseStream<T> {
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        unimplemented!()
    }
}
