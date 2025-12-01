// ... (imports remain)
use crate::send_future::SendFuture;
use crate::server::message::AsMut;
use crate::Status;

/// A trait for lazily resolving and mutating a request message.
///
/// This is a core component of Tonic's unified request architecture,
/// allowing mutations to be deferred until the message is actually needed.
/// This enables zero-copy patterns and flexible handling of request bodies.
#[trait_variant::make(Send)]
pub trait Lazy<Req>: Send
where
    Req: AsMut,
{
    async fn resolve(self, target: <Req as AsMut>::Mut<'_>) -> Result<(), Status>;
}

/// A trait for mapping or applying further mutations to a request message asynchronously.
///
/// This is typically used to chain operations without boxing or allocating closures.
#[trait_variant::make(Send)]
pub trait Mapper<Req>: Send
where
    Req: AsMut,
{
    async fn map(self, target: <Req as AsMut>::Mut<'_>) -> Result<(), Status>;
}

// --- Zero-Cost Map Combinator ---

/// A zero-cost combinator that chains a `Lazy` implementation with an asynchronous `Mapper`.
///
/// This struct is created by the [`LazyExt::then`] method.
pub struct MapLazy<L, M> {
    pub inner: L,
    pub mapper: M,
}

impl<Req, L, M> Lazy<Req> for MapLazy<L, M>
where
    Req: AsMut,
    L: Lazy<Req>,
    M: Mapper<Req>,
{
    async fn resolve(self, mut target: <Req as AsMut>::Mut<'_>) -> Result<(), Status> {
        let inner_target = <Req as AsMut>::reborrow_view(&mut target);
        self.inner.resolve(inner_target).make_send().await?;
        self.mapper.map(target).make_send().await
    }
}

// --- Sync Map Combinator (Sugar) ---

/// A combinator that chains a `Lazy` implementation with a synchronous closure.
///
/// This struct is created by the [`LazyExt::map`] method.
pub struct SyncMapLazy<L, F> {
    pub inner: L,
    pub f: F,
}

impl<Req, L, F> Lazy<Req> for SyncMapLazy<L, F>
where
    Req: AsMut,
    L: Lazy<Req>,
    F: FnOnce(<Req as AsMut>::Mut<'_>) -> Result<(), Status> + Send,
{
    async fn resolve(self, mut target: <Req as AsMut>::Mut<'_>) -> Result<(), Status> {
        let inner_target = <Req as AsMut>::reborrow_view(&mut target);
        self.inner.resolve(inner_target).make_send().await?;
        (self.f)(target)
    }
}

/// Extension trait providing combinators for `Lazy` implementations.
pub trait LazyExt<Req>: Lazy<Req> + Sized
where
    Req: AsMut,
{
    /// Chains this lazy operation with an asynchronous `Mapper`.
    ///
    /// This produces a new `Lazy` that runs the initial resolution and then applies
    /// the asynchronous mapper, avoiding allocations for the combined operation.
    fn then<M>(self, mapper: M) -> MapLazy<Self, M> {
        MapLazy {
            inner: self,
            mapper,
        }
    }

    /// Chains this lazy operation with a synchronous closure.
    ///
    /// This is syntactic sugar to apply synchronous mutations after the initial
    /// resolution completes.
    fn map<F>(self, f: F) -> SyncMapLazy<Self, F>
    where
        F: FnOnce(<Req as AsMut>::Mut<'_>) -> Result<(), Status> + Send,
    {
        SyncMapLazy { inner: self, f }
    }
}

impl<Req, L: Lazy<Req>> LazyExt<Req> for L where Req: AsMut {}

// --- Test Usage ---

#[cfg(test)]
mod tests {
    use super::*;
    use protobuf_well_known_types::Timestamp;

    // A struct for our lazy logic (Zero Cost)
    struct SetSeconds {
        value: i64,
    }

    impl Lazy<Timestamp> for SetSeconds {
        async fn resolve(self, mut target: <Timestamp as AsMut>::Mut<'_>) -> Result<(), Status> {
            target.set_seconds(self.value);
            Ok(())
        }
    }

    // A struct for our mapping logic (Zero Cost)
    struct AddFive;
    impl Mapper<Timestamp> for AddFive {
        async fn map(self, mut target: <Timestamp as AsMut>::Mut<'_>) -> Result<(), Status> {
            let current = target.seconds();
            target.set_seconds(current + 5);
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_zero_allocation_chain() {
        // 1. Create the base lazy (Struct, not closure)
        let lazy = SetSeconds { value: 10 };

        // 2. Map it with a struct mapper
        let chained = lazy.then(AddFive);

        let mut msg = Timestamp::new();

        chained.resolve(msg.as_mut()).await.unwrap();

        assert_eq!(msg.seconds(), 15);
    }

    #[tokio::test]
    async fn test_sync_closure_sugar() {
        // 1. Create the base lazy
        let lazy = SetSeconds { value: 10 };

        // 2. Map it with a sync closure (Sugar)
        let chained = lazy.map(|mut target| {
            let current = target.seconds();
            target.set_seconds(current + 20);
            Ok(())
        });

        let mut msg = Timestamp::new();
        // ZeroBox allocation! Sync closures are easy.
        chained.resolve(msg.as_mut()).await.unwrap();

        assert_eq!(msg.seconds(), 30);
    }
}
