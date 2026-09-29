/// Generated client implementations.
pub mod route_guide_client {
    #![allow(unused_imports, dead_code, missing_docs, clippy::wildcard_imports)]
    use grpc::client::*;
    use grpc_protobuf::*;
    use grpc_protobuf::client::*;
    /// Interface exported by the server.
    #[derive(Debug, Clone)]
    pub struct RouteGuideClient<T> {
        channel: T,
    }
    impl<T> RouteGuideClient<T>
    where
        T: grpc::client::Invoke,
    {
        pub fn new(channel: T) -> Self {
            Self { channel }
        }
        /// A simple RPC.
        ///
        /// Obtains the feature at a given position.
        ///
        /// A feature with an empty name is returned if there's no feature at the given
        /// position.
        pub fn get_feature<ReqMsgView>(
            &self,
            request: ReqMsgView,
        ) -> UnaryCallBuilder<'_, &T, ReqMsgView, super::Feature>
        where
            ReqMsgView: protobuf::AsView<Proxied = super::Point> + Send + Sync,
        {
            UnaryCallBuilder::new(
                &self.channel,
                "/routeguide.RouteGuide/GetFeature",
                request,
            )
        }
        /// A server-to-client streaming RPC.
        ///
        /// Obtains the Features available within the given Rectangle.  Results are
        /// streamed rather than returned at once (e.g. in a response message with a
        /// repeated field), as the rectangle may cover a large area and contain a
        /// huge number of features.
        pub fn list_features<ReqMsgView>(
            &self,
            request: ReqMsgView,
        ) -> ServerStreamingCallBuilder<'_, &T, ReqMsgView, super::Feature>
        where
            ReqMsgView: protobuf::AsView<Proxied = super::Rectangle> + Send + Sync,
        {
            ServerStreamingCallBuilder::new(
                &self.channel,
                "/routeguide.RouteGuide/ListFeatures",
                request,
            )
        }
        /// A client-to-server streaming RPC.
        ///
        /// Accepts a stream of Points on a route being traversed, returning a
        /// RouteSummary when traversal is completed.
        pub fn record_route(
            &self,
        ) -> ClientStreamingCallBuilder<'_, &T, super::Point, super::RouteSummary> {
            ClientStreamingCallBuilder::new(
                &self.channel,
                "/routeguide.RouteGuide/RecordRoute",
            )
        }
        /// A Bidirectional streaming RPC.
        ///
        /// Accepts a stream of RouteNotes sent while a route is being traversed,
        /// while receiving other RouteNotes (e.g. from other users).
        pub fn route_chat(
            &self,
        ) -> BidiCallBuilder<'_, &T, super::RouteNote, super::RouteNote> {
            BidiCallBuilder::new(&self.channel, "/routeguide.RouteGuide/RouteChat")
        }
    }
}
/// Generated server implementations.
pub mod route_guide_server {
    #![allow(unused_variables, dead_code, missing_docs, clippy::wildcard_imports)]
    mod method_wrappers {
        pub(super) struct GetFeature<T> {
            pub(super) service: std::sync::Arc<T>,
        }
        impl<T: super::RouteGuide> grpc_protobuf::server::UnaryMethod for GetFeature<T> {
            type Request = super::super::Point;
            type Response = super::super::Feature;
            async fn call(
                &self,
                request: <Self::Request as protobuf::Proxied>::View<'_>,
                response: <Self::Response as protobuf::MutProxied>::Mut<'_>,
            ) -> grpc_protobuf::ServerStatus {
                self.service.get_feature(request, response).await
            }
        }
        pub(super) struct ListFeatures<T> {
            pub(super) service: std::sync::Arc<T>,
        }
        impl<T: super::RouteGuide> grpc_protobuf::server::ServerStreamingMethod
        for ListFeatures<T> {
            type Request = super::super::Rectangle;
            type Response = super::super::Feature;
            async fn call(
                &self,
                request: <Self::Request as protobuf::Proxied>::View<'_>,
                responses: grpc_protobuf::server::GrpcStreamingResponse<
                    '_,
                    super::super::Feature,
                >,
            ) -> grpc_protobuf::ServerStatus {
                self.service.list_features(request, responses).await
            }
        }
        pub(super) struct RecordRoute<T> {
            pub(super) service: std::sync::Arc<T>,
        }
        impl<T: super::RouteGuide> grpc_protobuf::server::ClientStreamingMethod
        for RecordRoute<T> {
            type Request = super::super::Point;
            type Response = super::super::RouteSummary;
            async fn call(
                &self,
                requests: grpc_protobuf::server::GrpcStreamingRequest<
                    super::super::Point,
                >,
                response: <Self::Response as protobuf::MutProxied>::Mut<'_>,
            ) -> grpc_protobuf::ServerStatus {
                self.service.record_route(requests, response).await
            }
        }
        pub(super) struct RouteChat<T> {
            pub(super) service: std::sync::Arc<T>,
        }
        impl<T: super::RouteGuide> grpc_protobuf::server::BidiStreamingMethod
        for RouteChat<T> {
            type Request = super::super::RouteNote;
            type Response = super::super::RouteNote;
            async fn call(
                &self,
                requests: grpc_protobuf::server::GrpcStreamingRequest<
                    super::super::RouteNote,
                >,
                responses: grpc_protobuf::server::GrpcStreamingResponse<
                    '_,
                    super::super::RouteNote,
                >,
            ) -> grpc_protobuf::ServerStatus {
                self.service.route_chat(requests, responses).await
            }
        }
    }
    /// Generated trait containing gRPC methods that should be implemented for use with RouteGuideServer.
    #[grpc::async_trait]
    pub trait RouteGuide: std::marker::Send + std::marker::Sync + 'static {
        /// A simple RPC.
        ///
        /// Obtains the feature at a given position.
        ///
        /// A feature with an empty name is returned if there's no feature at the given
        /// position.
        async fn get_feature(
            &self,
            request: super::PointView<'_>,
            response: super::FeatureMut<'_>,
        ) -> grpc_protobuf::ServerStatus {
            Err(
                grpc_protobuf::ServerStatusError::new(
                    grpc_protobuf::StatusCodeError::Unimplemented,
                    "Not yet implemented",
                ),
            )
        }
        /// A server-to-client streaming RPC.
        ///
        /// Obtains the Features available within the given Rectangle.  Results are
        /// streamed rather than returned at once (e.g. in a response message with a
        /// repeated field), as the rectangle may cover a large area and contain a
        /// huge number of features.
        async fn list_features(
            &self,
            request: super::RectangleView<'_>,
            responses: grpc_protobuf::server::GrpcStreamingResponse<'_, super::Feature>,
        ) -> grpc_protobuf::ServerStatus {
            Err(
                grpc_protobuf::ServerStatusError::new(
                    grpc_protobuf::StatusCodeError::Unimplemented,
                    "Not yet implemented",
                ),
            )
        }
        /// A client-to-server streaming RPC.
        ///
        /// Accepts a stream of Points on a route being traversed, returning a
        /// RouteSummary when traversal is completed.
        async fn record_route(
            &self,
            request: grpc_protobuf::server::GrpcStreamingRequest<super::Point>,
            response: super::RouteSummaryMut<'_>,
        ) -> grpc_protobuf::ServerStatus {
            Err(
                grpc_protobuf::ServerStatusError::new(
                    grpc_protobuf::StatusCodeError::Unimplemented,
                    "Not yet implemented",
                ),
            )
        }
        /// A Bidirectional streaming RPC.
        ///
        /// Accepts a stream of RouteNotes sent while a route is being traversed,
        /// while receiving other RouteNotes (e.g. from other users).
        async fn route_chat(
            &self,
            request: grpc_protobuf::server::GrpcStreamingRequest<super::RouteNote>,
            responses: grpc_protobuf::server::GrpcStreamingResponse<'_, super::RouteNote>,
        ) -> grpc_protobuf::ServerStatus {
            Err(
                grpc_protobuf::ServerStatusError::new(
                    grpc_protobuf::StatusCodeError::Unimplemented,
                    "Not yet implemented",
                ),
            )
        }
    }
    /// Interface exported by the server.
    #[derive(Debug)]
    pub struct RouteGuideServer<T> {
        inner: std::sync::Arc<T>,
    }
    impl<T> RouteGuideServer<T> {
        pub fn new(inner: T) -> Self {
            Self::from_arc(std::sync::Arc::new(inner))
        }
        pub fn from_arc(inner: std::sync::Arc<T>) -> Self {
            Self { inner }
        }
    }
    impl<T: RouteGuide> grpc::server::service::Service for RouteGuideServer<T> {
        fn descriptor(&self) -> grpc::server::descriptor::ServiceDescriptor {
            grpc::server::descriptor::ServiceDescriptor::new(
                "routeguide.RouteGuide",
                vec![
                    grpc::server::descriptor::MethodDescriptor::new("/routeguide.RouteGuide/GetFeature"),
                    grpc::server::descriptor::MethodDescriptor::new("/routeguide.RouteGuide/ListFeatures"),
                    grpc::server::descriptor::MethodDescriptor::new("/routeguide.RouteGuide/RecordRoute"),
                    grpc::server::descriptor::MethodDescriptor::new("/routeguide.RouteGuide/RouteChat"),
                ],
            )
        }
        fn register_methods(
            self,
        ) -> std::vec::Vec<
            (std::string::String, std::sync::Arc<dyn grpc::server::DynHandle>),
        > {
            vec![
                ("/routeguide.RouteGuide/GetFeature".to_string(),
                std::sync::Arc::new(grpc_protobuf::server::UnaryAdapter::new(method_wrappers::GetFeature
                { service : self.inner.clone(), })),),
                ("/routeguide.RouteGuide/ListFeatures".to_string(),
                std::sync::Arc::new(grpc_protobuf::server::ServerStreamingAdapter::new(method_wrappers::ListFeatures
                { service : self.inner.clone(), })),),
                ("/routeguide.RouteGuide/RecordRoute".to_string(),
                std::sync::Arc::new(grpc_protobuf::server::ClientStreamingAdapter::new(method_wrappers::RecordRoute
                { service : self.inner.clone(), })),),
                ("/routeguide.RouteGuide/RouteChat".to_string(),
                std::sync::Arc::new(grpc_protobuf::server::BidiStreamingAdapter::new(method_wrappers::RouteChat
                { service : self.inner.clone(), })),),
            ]
        }
    }
}
