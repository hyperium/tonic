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

use crate::client::CallOptions;
use crate::core::RequestHeaders;
use crate::core::Trailers;
use crate::server::Handle;
use crate::server::RecvStream;
use crate::server::SendStream;

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
    async fn handle(
        &self,
        headers: RequestHeaders,
        options: CallOptions,
        tx: &mut impl SendStream,
        rx: impl RecvStream + 'static,
    ) -> Trailers {
        self.intercept
            .intercept(headers, options, tx, rx, &self.handle)
            .await
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

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use tokio::sync::Mutex;

    use super::*;
    use crate::client::CallOptions;
    use crate::core::RecvMessage;
    use crate::core::RequestHeaders;
    use crate::server::ResponseStreamItem;
    use crate::server::SendOptions;

    struct MockSendStream;
    impl SendStream for MockSendStream {
        async fn send<'a>(
            &mut self,
            _item: ResponseStreamItem<'a>,
            _options: SendOptions,
        ) -> Result<(), ()> {
            Ok(())
        }
    }

    struct MockRecvStream;
    impl RecvStream for MockRecvStream {
        async fn next(&mut self, _msg: &mut dyn RecvMessage) -> Option<Result<(), ()>> {
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
                RequestHeaders::default(),
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
                RequestHeaders::default(),
                CallOptions::default(),
                &mut tx,
                rx,
            )
            .await;

        let final_order = order.lock().await;
        assert_eq!(*final_order, vec![1, 2, 0]);
    }
}
