/// Generated client implementations.
pub mod route_guide_client {
    use grpc::client::*;
    use grpc_protobuf::*;
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
