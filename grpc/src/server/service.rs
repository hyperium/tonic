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

use std::sync::Arc;

use crate::server::DynHandle;
use crate::server::DynHandleWrapper;
use crate::server::descriptor::ServiceDescriptor;
use crate::server::interceptor::HandleExt;
use crate::server::interceptor::Intercept;

/// A gRPC service that can register its methods with a server router.
///
/// Implementations return their descriptor metadata via [`descriptor()`](Service::descriptor)
/// and produce their method handlers via [`register_methods()`](Service::register_methods).
pub trait Service: Send + 'static {
    /// Returns the service descriptor (pure metadata).
    ///
    /// This provides service and method metadata without registering handlers,
    /// enabling use cases like server reflection and service listing.
    fn descriptor(&self) -> ServiceDescriptor;

    /// Produces all method handlers for this service as type-erased dynamic handlers
    /// paired with their full method path (e.g. `"/mypackage.Echo/UnaryEcho"`).
    fn register_methods(self) -> Vec<(String, Arc<dyn DynHandle>)>;
}

/// A service wrapped with an interceptor that applies to all its methods.
///
/// Created by [`ServiceExt::with_interceptor()`]. The interceptor is applied
/// to each method handler at registration time.
pub struct InterceptedService<S, I> {
    service: S,
    interceptor: I,
}

impl<S, I> Service for InterceptedService<S, I>
where
    S: Service,
    I: Intercept + Clone + Send + Sync + 'static,
{
    fn descriptor(&self) -> ServiceDescriptor {
        self.service.descriptor()
    }

    fn register_methods(self) -> Vec<(String, Arc<dyn DynHandle>)> {
        let methods = self.service.register_methods();
        methods
            .into_iter()
            .map(|(path, handler)| {
                let intercepted =
                    DynHandleWrapper(handler).with_interceptor(self.interceptor.clone());
                (path, Arc::new(intercepted) as Arc<dyn DynHandle>)
            })
            .collect()
    }
}

/// Extension trait for composing interceptors on services.
pub trait ServiceExt: Service + Sized {
    /// Wraps this service with an interceptor that applies to all its methods.
    ///
    /// This is a pre-registration transformation: the interceptor is applied
    /// when the service registers its methods, not at call time.
    ///
    /// Equivalent to Java's `ServerInterceptors.intercept(service, interceptor)`.
    fn with_interceptor<I: Intercept>(self, interceptor: I) -> InterceptedService<Self, I> {
        InterceptedService {
            service: self,
            interceptor,
        }
    }
}

impl<T: Service> ServiceExt for T {}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::Mutex;

    use super::*;
    use crate::core::RecvMessage;
    use crate::core::StreamEnded;
    use crate::core::StreamError;
    use crate::core::test_connection_info;
    use crate::server::CallOptions;
    use crate::server::Handle;
    use crate::server::RecvStream;
    use crate::server::RequestHeaders;
    use crate::server::ResponseStreamItem;
    use crate::server::SendOptions;
    use crate::server::SendStream;
    use crate::server::Trailers;
    use crate::server::descriptor::MethodDescriptor;
    use crate::server::descriptor::ServiceDescriptor;
    use crate::server::interceptor::Intercept;
    use crate::server::router::RouterBuilder;

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

    /// An interceptor that records that it ran by pushing its id.
    #[derive(Clone)]
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

    /// A handler that pushes 0 to confirm it was called.
    struct TrackingHandler {
        order: Arc<Mutex<Vec<usize>>>,
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
            Trailers::new(Ok(()))
        }
    }

    /// A mock service that registers one method with a tracking handler.
    struct MockService {
        order: Arc<Mutex<Vec<usize>>>,
    }

    impl Service for MockService {
        fn descriptor(&self) -> ServiceDescriptor {
            ServiceDescriptor::new(
                "test.MockService",
                vec![MethodDescriptor::new("/test.MockService/Method")],
            )
        }

        fn register_methods(self) -> Vec<(String, Arc<dyn DynHandle>)> {
            vec![(
                "/test.MockService/Method".to_string(),
                Arc::new(TrackingHandler { order: self.order }),
            )]
        }
    }

    #[test]
    fn intercepted_service_preserves_descriptor() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let svc = MockService {
            order: order.clone(),
        };
        let interceptor = TrackingInterceptor {
            id: 1,
            order: order.clone(),
        };

        let intercepted = svc.with_interceptor(interceptor);
        let desc = intercepted.descriptor();
        assert_eq!(desc.name(), "test.MockService");
        assert_eq!(desc.methods().len(), 1);
        assert_eq!(desc.methods()[0].full_path(), "/test.MockService/Method");
    }

    #[tokio::test]
    async fn intercepted_service_applies_interceptor_to_handler() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let svc = MockService {
            order: order.clone(),
        };
        let interceptor = TrackingInterceptor {
            id: 1,
            order: order.clone(),
        };

        // Wrap service with interceptor and register via RouterBuilder.
        let intercepted = svc.with_interceptor(interceptor);
        let router = RouterBuilder::new().add_service(intercepted).build();

        // Invoke the handler.
        let headers = RequestHeaders::new("/test.MockService/Method", test_connection_info());
        let mut tx = MockSendStream;
        let rx = MockRecvStream;
        let trailers = router
            .handle(headers, CallOptions::default(), &mut tx, rx)
            .await;

        assert!(trailers.status().is_ok());
        // Interceptor (1) should run before handler (0).
        assert_eq!(*order.lock().await, vec![1, 0]);
    }

    #[tokio::test]
    async fn intercepted_service_chains_multiple_interceptors() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let svc = MockService {
            order: order.clone(),
        };
        let int_a = TrackingInterceptor {
            id: 1,
            order: order.clone(),
        };
        let int_b = TrackingInterceptor {
            id: 2,
            order: order.clone(),
        };

        // Chain: service -> int_a -> int_b
        // Execution: int_b runs first (outermost), then int_a, then handler.
        let intercepted = svc.with_interceptor(int_a).with_interceptor(int_b);
        let router = RouterBuilder::new().add_service(intercepted).build();

        let headers = RequestHeaders::new("/test.MockService/Method", test_connection_info());
        let mut tx = MockSendStream;
        let rx = MockRecvStream;
        let trailers = router
            .handle(headers, CallOptions::default(), &mut tx, rx)
            .await;

        assert!(trailers.status().is_ok());
        // Outermost interceptor (2) runs first, then (1), then handler (0).
        assert_eq!(*order.lock().await, vec![2, 1, 0]);
    }

    #[tokio::test]
    async fn service_without_interceptor_handler_runs_directly() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let svc = MockService {
            order: order.clone(),
        };

        let router = RouterBuilder::new().add_service(svc).build();

        let headers = RequestHeaders::new("/test.MockService/Method", test_connection_info());
        let mut tx = MockSendStream;
        let rx = MockRecvStream;
        let trailers = router
            .handle(headers, CallOptions::default(), &mut tx, rx)
            .await;

        assert!(trailers.status().is_ok());
        // Only handler (0), no interceptors.
        assert_eq!(*order.lock().await, vec![0]);
    }
}
