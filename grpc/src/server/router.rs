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

use std::collections::HashMap;
use std::sync::Arc;

use crate::StatusCodeError;
use crate::StatusError;
use crate::server::BoxedRecvStream;
use crate::server::CallOptions;
use crate::server::DynHandle;
use crate::server::DynSendStream;
use crate::server::Handle;
use crate::server::RecvStream;
use crate::server::RequestHeaders;
use crate::server::SendStream;
use crate::server::Trailers;
use crate::server::descriptor::MethodDescriptor;
use crate::server::descriptor::ServiceDescriptor;
use crate::server::interceptor::Identity;
use crate::server::interceptor::Intercept;
use crate::server::interceptor::InterceptExt;
use crate::server::interceptor::InterceptorChain;
use crate::server::service::Service;

/// A builder for constructing an immutable [`Router`].
pub(crate) struct RouterBuilder<I> {
    handlers: HashMap<String, Arc<dyn DynHandle>>,
    descriptors: Vec<ServiceDescriptor>,
    interceptor: I,
}

impl RouterBuilder<Identity> {
    /// Creates a new, empty `RouterBuilder` with no interceptors.
    pub(crate) fn new() -> RouterBuilder<Identity> {
        RouterBuilder {
            handlers: HashMap::new(),
            descriptors: Vec::new(),
            interceptor: Identity,
        }
    }
}

impl<I> RouterBuilder<I>
where
    I: Intercept + Clone + Send + Sync + 'static,
{
    /// Chains an additional interceptor after the existing stack.
    pub(crate) fn chain_interceptor<J>(self, next: J) -> RouterBuilder<InterceptorChain<I, J>>
    where
        J: Intercept + Clone + Send + Sync + 'static,
    {
        RouterBuilder {
            handlers: self.handlers,
            descriptors: self.descriptors,
            interceptor: self.interceptor.chain(next),
        }
    }

    /// Registers `handler` for the given method (last-one-wins on duplicate paths).
    pub(crate) fn add_method<H>(mut self, descriptor: MethodDescriptor, handler: H) -> Self
    where
        H: Handle + Send + Sync + 'static,
    {
        self.handlers
            .insert(descriptor.into_full_path(), Arc::new(handler));
        self
    }

    /// Registers all methods from a [`Service`].
    pub(crate) fn add_service(mut self, service: impl Service) -> Self {
        self.descriptors.push(service.descriptor());
        for (path, handler) in service.register_methods() {
            self.handlers.insert(path, handler);
        }
        self
    }

    /// Consumes this builder and produces an immutable [`Router`].
    pub(crate) fn build(self) -> Router<I> {
        Router {
            handlers: self.handlers,
            interceptor: self.interceptor,
        }
    }
}

impl Default for RouterBuilder<Identity> {
    fn default() -> Self {
        Self::new()
    }
}

/// Routes incoming gRPC RPCs to the correct handler based on the request's
/// method name.
///
/// `Router` implements [`Handle`], so it can be used as the handler for a
/// [`Server`](crate::server::Server). For most use cases, prefer
/// [`Server::builder()`](crate::server::Server::builder) which constructs
/// both the router and server together.
///
/// A `Router` is immutable once built; use [`RouterBuilder`] to construct one.
pub struct Router<I> {
    handlers: HashMap<String, Arc<dyn DynHandle>>,
    interceptor: I,
}

impl<I> Handle for Router<I>
where
    I: Intercept + Send + Sync + 'static,
{
    async fn handle(
        &self,
        headers: RequestHeaders,
        options: CallOptions,
        tx: &mut impl SendStream,
        rx: impl RecvStream + 'static,
    ) -> Trailers {
        // Resolve the method first: unknown methods short-circuit to
        // UNIMPLEMENTED without running the interceptor stack.
        let Some(handler) = self.handlers.get(headers.method_name()) else {
            return Trailers::new(Err(StatusError::new(
                StatusCodeError::Unimplemented,
                format!("unknown method: {}", headers.method_name()),
            )));
        };

        // Apply the global interceptor stack around the resolved handler.
        let next = DynHandleRef(handler.as_ref());
        self.interceptor
            .intercept(headers, options, tx, rx, &next)
            .await
    }
}

/// Adapts a type-erased [`DynHandle`] back into a [`Handle`] so the global
/// interceptor stack can invoke it as its `next` handler.
///
/// The only dynamic dispatch is the leaf `dyn_handle` call; the interceptor
/// stack itself remains monomorphized.
struct DynHandleRef<'a>(&'a dyn DynHandle);

impl Handle for DynHandleRef<'_> {
    async fn handle(
        &self,
        headers: RequestHeaders,
        options: CallOptions,
        tx: &mut impl SendStream,
        rx: impl RecvStream + 'static,
    ) -> Trailers {
        // Bridge from `impl SendStream` → `&mut dyn DynSendStream` and
        // `impl RecvStream` → `BoxedRecvStream`, matching the blanket
        // `DynHandle for T: Handle` pattern.
        let mut dyn_tx: &mut dyn DynSendStream = tx;
        let boxed_rx: BoxedRecvStream = Box::new(rx);
        self.0
            .dyn_handle(headers, options, &mut dyn_tx, boxed_rx)
            .await
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
    use crate::server::CallOptions;
    use crate::server::RequestHeaders;
    use crate::server::ResponseStreamItem;
    use crate::server::SendOptions;
    use crate::server::descriptor::MethodDescriptor;
    use crate::server::descriptor::ServiceDescriptor;

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

    /// A handler that records its method name when called and returns OK.
    struct RecordingHandler {
        called_with: Arc<Mutex<Option<String>>>,
    }

    impl Handle for RecordingHandler {
        async fn handle(
            &self,
            headers: RequestHeaders,
            _options: CallOptions,
            _tx: &mut impl SendStream,
            _rx: impl RecvStream + 'static,
        ) -> Trailers {
            let mut called = self.called_with.lock().await;
            *called = Some(headers.method_name().to_string());
            Trailers::new(Ok(()))
        }
    }

    #[tokio::test]
    async fn test_registered_method_dispatches() {
        let called_with = Arc::new(Mutex::new(None));
        let handler = RecordingHandler {
            called_with: called_with.clone(),
        };

        let router = RouterBuilder::new()
            .add_method(MethodDescriptor::new("/pkg.Svc/Method"), handler)
            .build();

        let headers = RequestHeaders::new("/pkg.Svc/Method", test_connection_info());

        let mut tx = MockSendStream;
        let rx = MockRecvStream;

        let trailers = router
            .handle(headers, CallOptions::default(), &mut tx, rx)
            .await;

        assert!(trailers.status().is_ok());
        assert_eq!(
            *called_with.lock().await,
            Some("/pkg.Svc/Method".to_string())
        );
    }

    #[tokio::test]
    async fn test_unregistered_method_returns_unimplemented() {
        let router = RouterBuilder::new().build();

        let headers = RequestHeaders::new("/pkg.Svc/NoSuchMethod", test_connection_info());

        let mut tx = MockSendStream;
        let rx = MockRecvStream;

        let trailers = router
            .handle(headers, CallOptions::default(), &mut tx, rx)
            .await;

        let err = trailers.status().as_ref().unwrap_err();
        assert_eq!(err.code(), StatusCodeError::Unimplemented);
        assert!(
            err.message().contains("/pkg.Svc/NoSuchMethod"),
            "error message should contain the method name, got: {}",
            err.message()
        );
    }

    #[tokio::test]
    async fn test_last_one_wins_for_duplicate_paths() {
        let first_called = Arc::new(Mutex::new(None));
        let second_called = Arc::new(Mutex::new(None));

        let first_handler = RecordingHandler {
            called_with: first_called.clone(),
        };
        let second_handler = RecordingHandler {
            called_with: second_called.clone(),
        };

        let router = RouterBuilder::new()
            .add_method(MethodDescriptor::new("/pkg.Svc/Method"), first_handler)
            .add_method(MethodDescriptor::new("/pkg.Svc/Method"), second_handler)
            .build();

        let headers = RequestHeaders::new("/pkg.Svc/Method", test_connection_info());

        let mut tx = MockSendStream;
        let rx = MockRecvStream;

        let trailers = router
            .handle(headers, CallOptions::default(), &mut tx, rx)
            .await;

        assert!(trailers.status().is_ok());
        // The second handler should have been called (last-one-wins).
        assert_eq!(
            *second_called.lock().await,
            Some("/pkg.Svc/Method".to_string())
        );
        // The first handler should NOT have been called.
        assert!(first_called.lock().await.is_none());
    }

    #[tokio::test]
    async fn test_add_service_visitor_pattern() {
        let method_a_called = Arc::new(Mutex::new(None));
        let method_b_called = Arc::new(Mutex::new(None));

        let handler_a = RecordingHandler {
            called_with: method_a_called.clone(),
        };
        let handler_b = RecordingHandler {
            called_with: method_b_called.clone(),
        };

        struct MockService {
            handler_a: RecordingHandler,
            handler_b: RecordingHandler,
        }

        impl Service for MockService {
            fn descriptor(&self) -> ServiceDescriptor {
                ServiceDescriptor::new(
                    "mock.MockService",
                    vec![
                        MethodDescriptor::new("/mock.MockService/MethodA"),
                        MethodDescriptor::new("/mock.MockService/MethodB"),
                    ],
                )
            }

            fn register_methods(self) -> Vec<(String, Arc<dyn DynHandle>)> {
                vec![
                    (
                        "/mock.MockService/MethodA".to_string(),
                        Arc::new(self.handler_a),
                    ),
                    (
                        "/mock.MockService/MethodB".to_string(),
                        Arc::new(self.handler_b),
                    ),
                ]
            }
        }

        let service = MockService {
            handler_a,
            handler_b,
        };

        let router = RouterBuilder::new().add_service(service).build();

        // Dispatch to MethodA.
        let headers_a = RequestHeaders::new("/mock.MockService/MethodA", test_connection_info());
        let mut tx = MockSendStream;
        let rx = MockRecvStream;
        let trailers = router
            .handle(headers_a, CallOptions::default(), &mut tx, rx)
            .await;
        assert!(trailers.status().is_ok());
        assert_eq!(
            *method_a_called.lock().await,
            Some("/mock.MockService/MethodA".to_string())
        );

        // Dispatch to MethodB.
        let headers_b = RequestHeaders::new("/mock.MockService/MethodB", test_connection_info());
        let mut tx = MockSendStream;
        let rx = MockRecvStream;
        let trailers = router
            .handle(headers_b, CallOptions::default(), &mut tx, rx)
            .await;
        assert!(trailers.status().is_ok());
        assert_eq!(
            *method_b_called.lock().await,
            Some("/mock.MockService/MethodB".to_string())
        );
    }

    #[tokio::test]
    async fn test_router_builder_default() {
        let router = RouterBuilder::default().build();

        let headers = RequestHeaders::new("/any.Service/AnyMethod", test_connection_info());

        let mut tx = MockSendStream;
        let rx = MockRecvStream;

        let trailers = router
            .handle(headers, CallOptions::default(), &mut tx, rx)
            .await;

        let err = trailers.status().as_ref().unwrap_err();
        assert_eq!(err.code(), StatusCodeError::Unimplemented);
    }

    // --- Interceptor-related test helpers ---

    use crate::server::interceptor::Intercept;

    /// An interceptor that pushes its `id` into a shared order vec, then
    /// delegates to `next`.
    #[derive(Clone)]
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
            self.order.lock().await.push(self.id);
            next.handle(headers, options, tx, rx).await
        }
    }

    /// A handler that pushes `0` into the shared order vec.
    struct OrderHandler {
        order: Arc<Mutex<Vec<usize>>>,
    }

    impl Handle for OrderHandler {
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

    #[tokio::test]
    async fn test_router_builder_with_single_interceptor() {
        let order = Arc::new(Mutex::new(Vec::new()));

        let interceptor = OrderInterceptor {
            id: 1,
            order: order.clone(),
        };
        let handler = OrderHandler {
            order: order.clone(),
        };

        let router = RouterBuilder::new()
            .chain_interceptor(interceptor)
            .add_method(MethodDescriptor::new("/pkg.Svc/Method"), handler)
            .build();

        let headers = RequestHeaders::new("/pkg.Svc/Method", test_connection_info());
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
    async fn test_router_builder_with_chained_interceptors() {
        let order = Arc::new(Mutex::new(Vec::new()));

        let auth = OrderInterceptor {
            id: 1,
            order: order.clone(),
        };
        let logging = OrderInterceptor {
            id: 2,
            order: order.clone(),
        };
        let handler = OrderHandler {
            order: order.clone(),
        };

        // First-added-first: auth (added first) should run before logging.
        let router = RouterBuilder::new()
            .chain_interceptor(auth)
            .chain_interceptor(logging)
            .add_method(MethodDescriptor::new("/pkg.Svc/Method"), handler)
            .build();

        let headers = RequestHeaders::new("/pkg.Svc/Method", test_connection_info());
        let mut tx = MockSendStream;
        let rx = MockRecvStream;

        let trailers = router
            .handle(headers, CallOptions::default(), &mut tx, rx)
            .await;

        assert!(trailers.status().is_ok());
        // Execution order: auth(1) → logging(2) → handler(0).
        assert_eq!(*order.lock().await, vec![1, 2, 0]);
    }

    #[tokio::test]
    async fn test_router_builder_interceptor_with_add_service() {
        let order = Arc::new(Mutex::new(Vec::new()));

        let interceptor = OrderInterceptor {
            id: 1,
            order: order.clone(),
        };

        struct InterceptedService {
            handler: OrderHandler,
        }

        impl Service for InterceptedService {
            fn descriptor(&self) -> ServiceDescriptor {
                ServiceDescriptor::new(
                    "test.InterceptedService",
                    vec![MethodDescriptor::new("/test.InterceptedService/Method")],
                )
            }

            fn register_methods(self) -> Vec<(String, Arc<dyn DynHandle>)> {
                vec![(
                    "/test.InterceptedService/Method".to_string(),
                    Arc::new(self.handler),
                )]
            }
        }

        let service = InterceptedService {
            handler: OrderHandler {
                order: order.clone(),
            },
        };

        let router = RouterBuilder::new()
            .chain_interceptor(interceptor)
            .add_service(service)
            .build();

        let headers =
            RequestHeaders::new("/test.InterceptedService/Method", test_connection_info());
        let mut tx = MockSendStream;
        let rx = MockRecvStream;

        let trailers = router
            .handle(headers, CallOptions::default(), &mut tx, rx)
            .await;

        assert!(trailers.status().is_ok());
        // Interceptor (1) should run before handler (0) even via add_service.
        assert_eq!(*order.lock().await, vec![1, 0]);
    }

    #[tokio::test]
    async fn test_global_interceptor_skipped_for_unknown_method() {
        let order = Arc::new(Mutex::new(Vec::new()));

        let interceptor = OrderInterceptor {
            id: 1,
            order: order.clone(),
        };

        // Global interceptor registered, but no handler for the requested method.
        let router = RouterBuilder::new()
            .chain_interceptor(interceptor)
            .add_method(
                MethodDescriptor::new("/pkg.Svc/Known"),
                OrderHandler {
                    order: order.clone(),
                },
            )
            .build();

        let headers = RequestHeaders::new("/pkg.Svc/Unknown", test_connection_info());
        let mut tx = MockSendStream;
        let rx = MockRecvStream;

        let trailers = router
            .handle(headers, CallOptions::default(), &mut tx, rx)
            .await;

        // Unknown method short-circuits to UNIMPLEMENTED...
        assert_eq!(
            trailers.status().as_ref().unwrap_err().code(),
            StatusCodeError::Unimplemented
        );
        // ...without running the global interceptor (resolve-then-intercept).
        assert!(order.lock().await.is_empty());
    }

    #[tokio::test]
    async fn test_recallable_interceptor_accumulates() {
        let order = Arc::new(Mutex::new(Vec::new()));

        let first = OrderInterceptor {
            id: 1,
            order: order.clone(),
        };
        let second = OrderInterceptor {
            id: 2,
            order: order.clone(),
        };
        let handler = OrderHandler {
            order: order.clone(),
        };

        // Two separate chain_interceptor calls accumulate; first added runs first.
        let router = RouterBuilder::new()
            .chain_interceptor(first)
            .chain_interceptor(second)
            .add_method(MethodDescriptor::new("/pkg.Svc/Method"), handler)
            .build();

        let headers = RequestHeaders::new("/pkg.Svc/Method", test_connection_info());
        let mut tx = MockSendStream;
        let rx = MockRecvStream;

        let trailers = router
            .handle(headers, CallOptions::default(), &mut tx, rx)
            .await;

        assert!(trailers.status().is_ok());
        // Execution order: first(1) → second(2) → handler(0).
        assert_eq!(*order.lock().await, vec![1, 2, 0]);
    }

    #[tokio::test]
    async fn test_global_runs_before_per_service_interceptor() {
        use crate::server::service::ServiceExt;

        let order = Arc::new(Mutex::new(Vec::new()));

        let global = OrderInterceptor {
            id: 1,
            order: order.clone(),
        };
        let per_service = OrderInterceptor {
            id: 2,
            order: order.clone(),
        };

        struct SingleMethodService {
            handler: OrderHandler,
        }

        impl Service for SingleMethodService {
            fn descriptor(&self) -> ServiceDescriptor {
                ServiceDescriptor::new("test.Svc", vec![MethodDescriptor::new("/test.Svc/Method")])
            }

            fn register_methods(self) -> Vec<(String, Arc<dyn DynHandle>)> {
                vec![("/test.Svc/Method".to_string(), Arc::new(self.handler))]
            }
        }

        let service = SingleMethodService {
            handler: OrderHandler {
                order: order.clone(),
            },
        }
        .with_interceptor(per_service);

        let router = RouterBuilder::new()
            .chain_interceptor(global)
            .add_service(service)
            .build();

        let headers = RequestHeaders::new("/test.Svc/Method", test_connection_info());
        let mut tx = MockSendStream;
        let rx = MockRecvStream;

        let trailers = router
            .handle(headers, CallOptions::default(), &mut tx, rx)
            .await;

        assert!(trailers.status().is_ok());
        // Global (1) runs outermost, then per-service (2), then handler (0).
        assert_eq!(*order.lock().await, vec![1, 2, 0]);
    }
}
