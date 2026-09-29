/// Generated client implementations.
pub mod greeter_client {
    #![allow(unused_imports, dead_code, missing_docs, clippy::wildcard_imports)]
    use grpc::client::*;
    use grpc_protobuf::*;
    use grpc_protobuf::client::*;
    /// The greeting service definition.
    #[derive(Debug, Clone)]
    pub struct GreeterClient<T> {
        channel: T,
    }
    impl<T> GreeterClient<T>
    where
        T: grpc::client::Invoke,
    {
        pub fn new(channel: T) -> Self {
            Self { channel }
        }
        /// Sends a greeting
        pub fn say_hello<ReqMsgView>(
            &self,
            request: ReqMsgView,
        ) -> UnaryCallBuilder<'_, &T, ReqMsgView, super::HelloReply>
        where
            ReqMsgView: protobuf::AsView<Proxied = super::HelloRequest> + Send + Sync,
        {
            UnaryCallBuilder::new(&self.channel, "/helloworld.Greeter/SayHello", request)
        }
    }
}
