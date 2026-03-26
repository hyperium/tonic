use crate::server::stream::stream_writer::{PushStreamConsumer, PushStreamWriter};
use crate::Status;
use std::future::Future;

/// Extension trait for `PushStreamWriter`.
pub trait PushStreamWriterExt<T>: Sized {
    type Output<U, F>;
    /// Maps items to be written to the stream using the provided async function.
    fn then<U, F, Fut>(self, f: F) -> Self::Output<U, F>
    where
        U: Send,
        F: FnMut(U) -> Fut + Send,
        Fut: Future<Output = Result<T, Status>> + Send;
}

impl<C, T> PushStreamWriterExt<T> for PushStreamWriter<C>
where
    T: Send,
    C: PushStreamConsumer<T> + Send,
{
    type Output<U, F> = PushStreamWriter<ThenConsumer<C, F>>;

    fn then<U, F, Fut>(self, f: F) -> Self::Output<U, F>

    where
        U: Send,
        F: FnMut(U) -> Fut + Send,
        Fut: Future<Output = Result<T, Status>> + Send,
    {
        let consumer = ThenConsumer {
            inner: self.into_inner(),
            f,
        };
        PushStreamWriter::new(consumer)
    }
}

#[doc(hidden)]
pub struct ThenConsumer<C, F> {
    inner: C,
    f: F,
}

impl<C, T, F, Fut, U> PushStreamConsumer<U> for ThenConsumer<C, F>
where
    C: PushStreamConsumer<T> + Send,
    T: Send,
    U: Send,
    F: FnMut(U) -> Fut + Send,
    Fut: Future<Output = Result<T, Status>> + Send,
{
    async fn write(&mut self, item: U) -> Result<(), Status> {
        let mapped = (self.f)(item).await?;
        self.inner.write(mapped).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

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

    impl<T: Send + Sync> PushStreamConsumer<T> for MockConsumer<T> {
        async fn write(&mut self, item: T) -> Result<(), Status> {
            self.items.lock().unwrap().push(item);
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_then_success() {
        let consumer = MockConsumer::new();
        let items = consumer.items.clone();
        let writer = PushStreamWriter::new(consumer);
        let mut mapped_writer = writer.then(|x: i32| async move { Ok(x.to_string()) });

        mapped_writer.write(1).await.unwrap();
        mapped_writer.write(2).await.unwrap();

        let items = items.lock().unwrap();
        assert_eq!(*items, vec!["1", "2"]);
    }

    #[tokio::test]
    async fn test_then_failure() {
        let consumer = MockConsumer::new();
        let items = consumer.items.clone();
        let writer = PushStreamWriter::new(consumer);
        let mut mapped_writer = writer.then(|x: i32| async move {
            if x == 2 {
                Err(Status::new(crate::status::StatusCode::Internal, "error"))
            } else {
                Ok(x.to_string())
            }
        });

        mapped_writer.write(1).await.unwrap();
        let result = mapped_writer.write(2).await;
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().code(),
            crate::status::StatusCode::Internal
        );

        let items = items.lock().unwrap();
        assert_eq!(*items, vec!["1"]);
    }
}
