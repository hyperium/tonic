use crate::server::stream::push_stream::{PushStream, PushStreamProducer};
use crate::server::stream::stream_writer::PushStreamWriter;
use crate::server::stream::stream_writer_ext::PushStreamWriterExt;
use crate::Status;
use std::future::Future;

/// Extension trait for `PushStream`.
pub trait PushStreamExt: Sized {
    type Item;
    type ThenStream<U, F>;

    /// Maps items produced by the stream using the provided async function.
    fn then<U, F, Fut>(self, f: F) -> Self::ThenStream<U, F>
    where
        U: Send,
        F: FnMut(Self::Item) -> Fut + Send,
        Fut: Future<Output = Result<U, Status>> + Send;
}

impl<P> PushStreamExt for PushStream<P>
where
    P: PushStreamProducer,
{
    type Item = P::Item;
    type ThenStream<U, F> = PushStream<ThenProducer<P, F>>;

    fn then<U, F, Fut>(self, f: F) -> Self::ThenStream<U, F>
    where
        U: Send,
        F: FnMut(Self::Item) -> Fut + Send,
        Fut: Future<Output = Result<U, Status>> + Send,
    {
        let producer = ThenProducer {
            inner: self.into_inner(),
            f,
        };
        PushStream::new(producer)
    }
}

#[doc(hidden)]
pub struct ThenProducer<P, F> {
    inner: P,
    f: F,
}

impl<P, F, Fut, U> PushStreamProducer for ThenProducer<P, F>
where
    P: PushStreamProducer,
    U: Send,
    F: FnMut(P::Item) -> Fut + Send,
    Fut: Future<Output = Result<U, Status>> + Send,
{
    type Item = U;

    async fn produce(
        self,
        writer: PushStreamWriter<impl crate::server::stream::PushStreamConsumer<Self::Item>>,
    ) -> Result<(), Status> {
        let writer = writer.then(self.f);
        self.inner.produce(writer).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::stream::push_stream::PushStream;
    use crate::server::stream::stream_writer::PushStreamConsumer;
    use std::sync::{Arc, Mutex};

    struct MockProducer {
        items: Vec<i32>,
    }

    impl PushStreamProducer for MockProducer {
        type Item = i32;

        async fn produce(
            self,
            mut writer: PushStreamWriter<impl crate::server::stream::PushStreamConsumer<Self::Item>,
            >,
        ) -> Result<(), Status> {
            for item in self.items {
                writer.write(item).await?;
            }
            Ok(())
        }
    }

    struct MockConsumer<T> {
        items: Arc<Mutex<Vec<T>>>,
    }

    impl<T> MockConsumer<T> {
        fn new() -> Self {
            Self {
                items: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl<T: Send> PushStreamConsumer<T> for MockConsumer<T> {
        async fn write(&mut self, item: T) -> Result<(), Status> {
            self.items.lock().unwrap().push(item);
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_then_success() {
        let producer = MockProducer {
            items: vec![1, 2, 3],
        };
        let stream = PushStream::new(producer);
        let mapped_stream = stream.then(|x| async move { Ok(x * 2) });

        let consumer = MockConsumer::new();
        let items = consumer.items.clone();
        let writer = PushStreamWriter::new(consumer);

        mapped_stream.run(writer).await.unwrap();

        let items = items.lock().unwrap();
        assert_eq!(*items, vec![2, 4, 6]);
    }

    #[tokio::test]
    async fn test_then_failure() {
        let producer = MockProducer {
            items: vec![1, 2, 3],
        };
        let stream = PushStream::new(producer);
        let mapped_stream = stream.then(|x| async move {
            if x == 2 {
                Err(Status::new(crate::status::StatusCode::Internal, "error"))
            } else {
                Ok(x * 2)
            }
        });

        let consumer = MockConsumer::new();
        let items = consumer.items.clone();
        let writer = PushStreamWriter::new(consumer);

        let result = mapped_stream.run(writer).await;
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().code(),
            crate::status::StatusCode::Internal
        );

        let items = items.lock().unwrap();
        // Should have processed 1 -> 2, then failed on 2
        assert_eq!(*items, vec![2]);
    }

    #[tokio::test]
    async fn test_then_async_sleep() {
        let producer = MockProducer {
            items: vec![1, 2, 3],
        };
        let stream = PushStream::new(producer);
        let mapped_stream = stream.then(|x| async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            Ok(x * 2)
        });

        let consumer = MockConsumer::new();
        let items = consumer.items.clone();
        let writer = PushStreamWriter::new(consumer);

        mapped_stream.run(writer).await.unwrap();

        let items = items.lock().unwrap();
        assert_eq!(*items, vec![2, 4, 6]);
    }
}
