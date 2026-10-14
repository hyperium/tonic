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

use crate::rt::GrpcRuntime;
use crate::server::Server;
use crate::server::interceptor::Identity;
use crate::server::interceptor::Intercept;
use crate::server::interceptor::InterceptorChain;
use crate::server::router::RouterBuilder;
use crate::server::service::Service;

/// A fluent builder for constructing a [`Server`].
///
/// Register services with [`add_service()`](ServerBuilder::add_service) and add
/// global interceptors with [`interceptor()`](ServerBuilder::interceptor), then
/// finish with [`build()`](ServerBuilder::build).
pub struct ServerBuilder<I = Identity> {
    router: RouterBuilder<I>,
}

// ---------------------------------------------------------------------------
// Constructor
// ---------------------------------------------------------------------------

impl ServerBuilder<Identity> {
    /// Creates a new `ServerBuilder` with no interceptors.
    pub(crate) fn new() -> Self {
        ServerBuilder {
            router: RouterBuilder::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Setters — available on any ServerBuilder<I>
// ---------------------------------------------------------------------------

impl<I: Intercept + Clone + Send + Sync + 'static> ServerBuilder<I> {
    /// Adds a global interceptor applied to every registered method.
    ///
    /// May be called repeatedly to compose multiple interceptors.
    pub fn interceptor<J>(self, next: J) -> ServerBuilder<InterceptorChain<I, J>>
    where
        J: Intercept + Clone + Send + Sync + 'static,
    {
        ServerBuilder {
            router: self.router.chain_interceptor(next),
        }
    }

    /// Registers all methods from a [`Service`].
    pub fn add_service(mut self, service: impl Service) -> Self {
        self.router = self.router.add_service(service);
        self
    }

    /// Builds the [`Server`] with the explicitly provided runtime.
    ///
    /// Always available. When the `_runtime-tokio` feature is enabled,
    /// [`build()`](ServerBuilder::build) can be used instead to use the default
    /// Tokio runtime.
    pub fn build_with_runtime(self, runtime: GrpcRuntime) -> Server {
        Server::new(self.router.build(), runtime)
    }
}

// ---------------------------------------------------------------------------
// build() — uses the default runtime when _runtime-tokio is enabled
// ---------------------------------------------------------------------------

#[cfg(feature = "_runtime-tokio")]
impl<I: Intercept + Clone + Send + Sync + 'static> ServerBuilder<I> {
    /// Builds the [`Server`] using the default Tokio runtime.
    ///
    /// Available only when the `_runtime-tokio` feature is enabled. Without it,
    /// use [`build_with_runtime()`](ServerBuilder::build_with_runtime).
    pub fn build(self) -> Server {
        Server::new(self.router.build(), crate::rt::default_runtime())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::Mutex;

    use crate::core::RecvMessage;
    use crate::core::StreamEnded;
    use crate::core::StreamError;
    use crate::server::CallOptions;
    use crate::server::DynHandle;
    use crate::server::Handle;
    use crate::server::RecvStream;
    use crate::server::RequestHeaders;
    use crate::server::ResponseStreamItem;
    use crate::server::SendOptions;
    use crate::server::SendStream;
    use crate::server::Server;
    use crate::server::Trailers;
    use crate::server::descriptor::MethodDescriptor;
    use crate::server::descriptor::ServiceDescriptor;
    use crate::server::interceptor::Intercept;
    use crate::server::interceptor::InterceptExt;
    use crate::server::service::Service;

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

    struct TestService {
        order: Arc<Mutex<Vec<usize>>>,
    }

    impl Service for TestService {
        fn descriptor(&self) -> ServiceDescriptor {
            ServiceDescriptor::new("test.Svc", vec![MethodDescriptor::new("/test.Svc/Method")])
        }

        fn register_methods(self) -> Vec<(String, Arc<dyn DynHandle>)> {
            vec![(
                "/test.Svc/Method".to_string(),
                Arc::new(TrackingHandler { order: self.order }),
            )]
        }
    }

    #[test]
    fn server_builder_builds_without_services() {
        // Should not panic — creates a server with empty router.
        let _server = Server::builder().build();
    }

    #[tokio::test]
    async fn server_builder_with_interceptor_chain_and_service() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let int_a = TrackingInterceptor {
            id: 1,
            order: order.clone(),
        };
        let int_b = TrackingInterceptor {
            id: 2,
            order: order.clone(),
        };
        let chain = int_a.chain(int_b);

        let svc = TestService {
            order: order.clone(),
        };

        let _server = Server::builder()
            .interceptor(chain)
            .add_service(svc)
            .build();
    }

    #[tokio::test]
    async fn server_builder_interceptor_method() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let int_a = TrackingInterceptor {
            id: 1,
            order: order.clone(),
        };
        let int_b = TrackingInterceptor {
            id: 2,
            order: order.clone(),
        };
        let chain = int_a.chain(int_b);

        let svc = TestService {
            order: order.clone(),
        };

        // Test the new .interceptor() API.
        let _server = Server::builder()
            .interceptor(chain)
            .add_service(svc)
            .build();
    }

    #[test]
    fn server_builder_multiple_services() {
        struct SvcA;
        impl Service for SvcA {
            fn descriptor(&self) -> ServiceDescriptor {
                ServiceDescriptor::new("test.A", vec![])
            }
            fn register_methods(self) -> Vec<(String, Arc<dyn DynHandle>)> {
                vec![]
            }
        }

        struct SvcB;
        impl Service for SvcB {
            fn descriptor(&self) -> ServiceDescriptor {
                ServiceDescriptor::new("test.B", vec![])
            }
            fn register_methods(self) -> Vec<(String, Arc<dyn DynHandle>)> {
                vec![]
            }
        }

        let _server = Server::builder()
            .add_service(SvcA)
            .add_service(SvcB)
            .build();
    }

    #[test]
    fn server_builder_with_explicit_runtime() {
        let rt = crate::rt::default_runtime();
        let _server = Server::builder().build_with_runtime(rt);
    }
}
