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

use crate::server::CallOptions;
use crate::server::Handle;
use crate::server::RecvStream;
use crate::server::RequestHeaders;
use crate::server::SendStream;
use crate::server::Trailers;

/// A trait which allows intercepting an incoming RPC call to a [`Handle`] implementation.
#[trait_variant::make(Send)]
pub trait Intercept: Sync + 'static {
    /// Intercepts an incoming call.
    ///
    /// Implementations can wrap `tx` and `rx` before passing them to `next`.
    async fn intercept(
        &self,
        headers: RequestHeaders,
        options: CallOptions,
        tx: &mut impl SendStream,
        rx: impl RecvStream + 'static,
        next: &impl Handle,
    ) -> Trailers;
}

/// Wraps a [`Handle`] and an [`Intercept`] and implements [`Handle`] for the combination.
pub struct Intercepted<H, I> {
    handle: H,
    intercept: I,
}

impl<H, I> Intercepted<H, I> {
    /// Creates a new `Intercepted` wrapper combining a handle and an interceptor.
    pub fn new(handle: H, intercept: I) -> Self {
        Self { handle, intercept }
    }
}

impl<H, I> Handle for Intercepted<H, I>
where
    H: Handle + 'static,
    I: Intercept + 'static,
{
    fn handle(
        &self,
        headers: RequestHeaders,
        options: CallOptions,
        tx: &mut impl SendStream,
        rx: impl RecvStream + 'static,
    ) -> impl std::future::Future<Output = Trailers> + Send {
        self.intercept
            .intercept(headers, options, tx, rx, &self.handle)
    }
}

/// Implements methods for combining [`Handle`] implementations with [`Intercept`] interceptors.
pub trait HandleExt: Handle + Sized {
    /// Wraps this [`Handle`] with the given [`Intercept`] interceptor.
    fn with_interceptor<I>(self, interceptor: I) -> Intercepted<Self, I>
    where
        I: Intercept,
    {
        Intercepted::new(self, interceptor)
    }
}

impl<T: Handle + Sized> HandleExt for T {}

/// A no-op interceptor that simply delegates to the next handler.
#[derive(Clone, Copy)]
pub struct Identity;

impl Intercept for Identity {
    fn intercept(
        &self,
        headers: RequestHeaders,
        options: CallOptions,
        tx: &mut impl SendStream,
        rx: impl RecvStream + 'static,
        next: &impl Handle,
    ) -> impl std::future::Future<Output = Trailers> + Send {
        next.handle(headers, options, tx, rx)
    }
}

/// Extension trait for chaining [`Intercept`] implementations.
///
/// Provides the [`chain`](InterceptExt::chain) method, which composes two
/// interceptors into a single [`InterceptorChain`] that itself implements
/// `Intercept`. Chains nest naturally via repeated calls.
pub trait InterceptExt: Intercept + Sized {
    /// Chains `self` with `next`, returning an [`InterceptorChain`] where
    /// `self` runs first and `next` runs second.
    fn chain<I: Intercept>(self, next: I) -> InterceptorChain<Self, I> {
        InterceptorChain {
            first: self,
            second: next,
        }
    }
}

impl<T: Intercept + Sized> InterceptExt for T {}

/// Two interceptors chained together, where `first` runs before `second`.
///
/// Created via [`InterceptExt::chain`]. Itself implements [`Intercept`], so
/// chains compose recursively:
/// `InterceptorChain<A, InterceptorChain<B, C>>` runs A → B → C.
#[derive(Clone)]
pub struct InterceptorChain<A, B> {
    first: A,
    second: B,
}

impl<A, B> Intercept for InterceptorChain<A, B>
where
    A: Intercept,
    B: Intercept,
{
    async fn intercept(
        &self,
        headers: RequestHeaders,
        options: CallOptions,
        tx: &mut impl SendStream,
        rx: impl RecvStream + 'static,
        next: &impl Handle,
    ) -> Trailers {
        // Build a temporary Handle that runs `second` then delegates to `next`.
        let inner = SecondThenNext {
            second: &self.second,
            next,
        };
        self.first.intercept(headers, options, tx, rx, &inner).await
    }
}

/// A temporary Handle adapter used inside [`InterceptorChain`].
///
/// When called, it runs `second.intercept(...)` with `next` as the
/// downstream handler, achieving the chained execution order.
struct SecondThenNext<'a, B, N: ?Sized> {
    second: &'a B,
    next: &'a N,
}

impl<B, N> Handle for SecondThenNext<'_, B, N>
where
    B: Intercept,
    N: Handle,
{
    fn handle(
        &self,
        headers: RequestHeaders,
        options: CallOptions,
        tx: &mut impl SendStream,
        rx: impl RecvStream + 'static,
    ) -> impl std::future::Future<Output = Trailers> + Send {
        self.second.intercept(headers, options, tx, rx, self.next)
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use tokio::sync::Mutex;

    use super::*;
    use crate::core::RecvMessage;
    use crate::core::StreamEnded;
    use crate::core::StreamError;
    use crate::core::test_connection_info;
    use crate::server::RequestHeaders;
    use crate::server::ResponseStreamItem;
    use crate::server::SendOptions;
    use crate::status::StatusCodeError;
    use crate::status::StatusError;

    struct MockSendStream;
    impl SendStream for MockSendStream {
        async fn send(
            &mut self,
            _item: ResponseStreamItem<'_>,
            _options: SendOptions,
        ) -> Result<(), StreamEnded> {
            Ok(())
        }
    }

    struct MockRecvStream;
    impl RecvStream for MockRecvStream {
        async fn next(&mut self, _msg: &mut dyn RecvMessage) -> Option<Result<(), StreamError>> {
            None
        }
    }

    struct MockHandler {
        called: Arc<Mutex<bool>>,
    }

    impl Handle for MockHandler {
        async fn handle(
            &self,
            _headers: RequestHeaders,
            _options: CallOptions,
            _tx: &mut impl SendStream,
            _rx: impl RecvStream + 'static,
        ) -> Trailers {
            let mut called = self.called.lock().await;
            *called = true;
            Trailers::new(Ok(()))
        }
    }

    struct MockInterceptor {
        called: Arc<Mutex<bool>>,
    }

    impl Intercept for MockInterceptor {
        async fn intercept(
            &self,
            headers: RequestHeaders,
            options: CallOptions,
            tx: &mut impl SendStream,
            rx: impl RecvStream + 'static,
            next: &impl Handle,
        ) -> Trailers {
            let mut called = self.called.lock().await;
            *called = true;
            drop(called);
            next.handle(headers, options, tx, rx).await
        }
    }

    #[tokio::test]
    async fn test_simple_interceptor() {
        let handler_called = Arc::new(Mutex::new(false));
        let interceptor_called = Arc::new(Mutex::new(false));

        let handler = MockHandler {
            called: handler_called.clone(),
        };
        let interceptor = MockInterceptor {
            called: interceptor_called.clone(),
        };

        let chain = handler.with_interceptor(interceptor);

        let mut tx = MockSendStream;
        let rx = MockRecvStream;

        chain
            .handle(
                RequestHeaders::new("", test_connection_info()),
                CallOptions::default(),
                &mut tx,
                rx,
            )
            .await;

        assert!(*interceptor_called.lock().await);
        assert!(*handler_called.lock().await);
    }

    #[tokio::test]
    async fn test_interceptor_chaining_order() {
        let order = Arc::new(Mutex::new(Vec::new()));

        struct OrderInterceptor {
            id: usize,
            order: Arc<Mutex<Vec<usize>>>,
        }

        impl Intercept for OrderInterceptor {
            async fn intercept(
                &self,
                headers: RequestHeaders,
                options: CallOptions,
                tx: &mut impl SendStream,
                rx: impl RecvStream + 'static,
                next: &impl Handle,
            ) -> Trailers {
                let mut order = self.order.lock().await;
                order.push(self.id);
                drop(order);
                next.handle(headers, options, tx, rx).await
            }
        }

        struct OrderHandler {
            order: Arc<Mutex<Vec<usize>>>,
        }

        impl Handle for OrderHandler {
            async fn handle(
                &self,
                _h: RequestHeaders,
                _o: CallOptions,
                _tx: &mut impl SendStream,
                _rx: impl RecvStream + 'static,
            ) -> Trailers {
                let mut order = self.order.lock().await;
                order.push(0); // 0 represents the handler
                Trailers::new(Ok(()))
            }
        }

        let handler = OrderHandler {
            order: order.clone(),
        };
        let int1 = OrderInterceptor {
            id: 1,
            order: order.clone(),
        };
        let int2 = OrderInterceptor {
            id: 2,
            order: order.clone(),
        };

        // This should run int1 first, then int2, then handler.
        let chain = handler.with_interceptor(int2).with_interceptor(int1);

        let mut tx = MockSendStream;
        let rx = MockRecvStream;

        chain
            .handle(
                RequestHeaders::new("", test_connection_info()),
                CallOptions::default(),
                &mut tx,
                rx,
            )
            .await;

        let final_order = order.lock().await;
        assert_eq!(*final_order, vec![1, 2, 0]);
    }

    // --- Chaining tests exercising `InterceptExt::chain` / `InterceptorChain` ---

    /// Records its `id` when run, then delegates to `next`.
    struct TrackingInterceptor {
        id: usize,
        order: Arc<Mutex<Vec<usize>>>,
    }

    impl Intercept for TrackingInterceptor {
        async fn intercept(
            &self,
            headers: RequestHeaders,
            options: CallOptions,
            tx: &mut impl SendStream,
            rx: impl RecvStream + 'static,
            next: &impl Handle,
        ) -> Trailers {
            self.order.lock().await.push(self.id);
            next.handle(headers, options, tx, rx).await
        }
    }

    /// Records `id` and returns without calling `next` (short-circuit).
    struct ShortCircuitInterceptor {
        id: usize,
        order: Arc<Mutex<Vec<usize>>>,
    }

    impl Intercept for ShortCircuitInterceptor {
        async fn intercept(
            &self,
            _headers: RequestHeaders,
            _options: CallOptions,
            _tx: &mut impl SendStream,
            _rx: impl RecvStream + 'static,
            _next: &impl Handle,
        ) -> Trailers {
            self.order.lock().await.push(self.id);
            Trailers::new(Ok(()))
        }
    }

    /// Records `0` (handler marker) and returns the given trailers.
    struct TrackingHandler {
        order: Arc<Mutex<Vec<usize>>>,
        trailers: Trailers,
    }

    impl Handle for TrackingHandler {
        async fn handle(
            &self,
            _headers: RequestHeaders,
            _options: CallOptions,
            _tx: &mut impl SendStream,
            _rx: impl RecvStream + 'static,
        ) -> Trailers {
            self.order.lock().await.push(0);
            self.trailers.clone()
        }
    }

    fn tracking_interceptor(id: usize, order: &Arc<Mutex<Vec<usize>>>) -> TrackingInterceptor {
        TrackingInterceptor {
            id,
            order: order.clone(),
        }
    }

    /// Invokes `chain.intercept(...)` against a `TrackingHandler` and returns the
    /// resulting `Trailers`.
    async fn run_chain(chain: &impl Intercept, handler: &impl Handle) -> Trailers {
        let mut tx = MockSendStream;
        let rx = MockRecvStream;
        chain
            .intercept(
                RequestHeaders::new("", test_connection_info()),
                CallOptions::default(),
                &mut tx,
                rx,
                handler,
            )
            .await
    }

    #[tokio::test]
    async fn chain_runs_first_then_second_then_handler() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let chain = tracking_interceptor(1, &order).chain(tracking_interceptor(2, &order));
        let handler = TrackingHandler {
            order: order.clone(),
            trailers: Trailers::new(Ok(())),
        };

        run_chain(&chain, &handler).await;

        assert_eq!(*order.lock().await, vec![1, 2, 0]);
    }

    #[tokio::test]
    async fn nested_chain_flattens_left_to_right() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let chain = tracking_interceptor(1, &order)
            .chain(tracking_interceptor(2, &order))
            .chain(tracking_interceptor(3, &order));
        let handler = TrackingHandler {
            order: order.clone(),
            trailers: Trailers::new(Ok(())),
        };

        run_chain(&chain, &handler).await;

        assert_eq!(*order.lock().await, vec![1, 2, 3, 0]);
    }

    #[tokio::test]
    async fn chain_of_chains() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let left = tracking_interceptor(1, &order).chain(tracking_interceptor(2, &order));
        let right = tracking_interceptor(3, &order).chain(tracking_interceptor(4, &order));
        let chain = left.chain(right);
        let handler = TrackingHandler {
            order: order.clone(),
            trailers: Trailers::new(Ok(())),
        };

        run_chain(&chain, &handler).await;

        assert_eq!(*order.lock().await, vec![1, 2, 3, 4, 0]);
    }

    #[tokio::test]
    async fn identity_is_transparent() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let chain = Identity.chain(tracking_interceptor(1, &order));
        let handler = TrackingHandler {
            order: order.clone(),
            trailers: Trailers::new(Ok(())),
        };

        run_chain(&chain, &handler).await;

        // Identity adds no entry; only interceptor 1 then the handler run.
        assert_eq!(*order.lock().await, vec![1, 0]);
    }

    #[tokio::test]
    async fn interceptor_can_short_circuit() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let chain = ShortCircuitInterceptor {
            id: 1,
            order: order.clone(),
        }
        .chain(tracking_interceptor(2, &order));
        let handler = TrackingHandler {
            order: order.clone(),
            trailers: Trailers::new(Ok(())),
        };

        run_chain(&chain, &handler).await;

        // The short-circuiting interceptor never calls `next`, so neither the
        // downstream interceptor (2) nor the handler (0) run.
        assert_eq!(*order.lock().await, vec![1]);
    }

    #[tokio::test]
    async fn trailers_propagate_back_through_chain() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let chain = tracking_interceptor(1, &order).chain(tracking_interceptor(2, &order));
        let distinctive = Trailers::new(Err(StatusError::new(
            StatusCodeError::FailedPrecondition,
            "from-handler",
        )));
        let handler = TrackingHandler {
            order: order.clone(),
            trailers: distinctive,
        };

        let result = run_chain(&chain, &handler).await;

        let err = result.into_status().unwrap_err();
        assert_eq!(err.code(), StatusCodeError::FailedPrecondition);
        assert_eq!(err.message(), "from-handler");
    }
}
