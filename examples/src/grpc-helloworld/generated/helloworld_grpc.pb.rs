/// Generated client implementations.
pub mod greeter_client {
    use grpc::client::*;
    use grpc_protobuf::*;

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
          ReqMsgView: protobuf::AsView<Proxied = super::HelloRequest> + Send + Sync {
          UnaryCallBuilder::new(&self.channel, "/helloworld.Greeter/SayHello", request)
        }
    }
}
