/// The message sent by the client when calling ServerReflectionInfo method.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ServerReflectionRequest {
    #[prost(string, tag = "1")]
    pub host: ::prost::alloc::string::String,
    /// To use reflection service, the client should set one of the following
    /// fields in message_request. The server distinguishes requests by their
    /// defined field and then handles them using corresponding methods.
    #[prost(
        oneof = "server_reflection_request::MessageRequest",
        tags = "3, 4, 5, 6, 7"
    )]
    pub message_request: ::core::option::Option<server_reflection_request::MessageRequest>,
}
/// Nested message and enum types in `ServerReflectionRequest`.
pub mod server_reflection_request {
    /// To use reflection service, the client should set one of the following
    /// fields in message_request. The server distinguishes requests by their
    /// defined field and then handles them using corresponding methods.
    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum MessageRequest {
        /// Find a proto file by the file name.
        #[prost(string, tag = "3")]
        FileByFilename(::prost::alloc::string::String),
        /// Find the proto file that declares the given fully-qualified symbol name.
        /// This field should be a fully-qualified symbol name
        /// (e.g. <package>.<service>[.<method>] or <package>.<type>).
        #[prost(string, tag = "4")]
        FileContainingSymbol(::prost::alloc::string::String),
        /// Find the proto file which defines an extension extending the given
        /// message type with the given field number.
        #[prost(message, tag = "5")]
        FileContainingExtension(super::ExtensionRequest),
        /// Finds the tag numbers used by all known extensions of extendee_type, and
        /// appends them to ExtensionNumberResponse in an undefined order.
        /// Its corresponding method is best-effort: it's not guaranteed that the
        /// reflection service will implement this method, and it's not guaranteed
        /// that this method will provide all extensions. Returns
        /// StatusCode::UNIMPLEMENTED if it's not implemented.
        /// This field should be a fully-qualified type name. The format is
        /// <package>.<type>
        #[prost(string, tag = "6")]
        AllExtensionNumbersOfType(::prost::alloc::string::String),
        /// List the full names of registered services. The content will not be
        /// checked.
        #[prost(string, tag = "7")]
        ListServices(::prost::alloc::string::String),
    }
}
/// The type name and extension number sent by the client when requesting
/// file_containing_extension.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ExtensionRequest {
    /// Fully-qualified type name. The format should be <package>.<type>
    #[prost(string, tag = "1")]
    pub containing_type: ::prost::alloc::string::String,
    #[prost(int32, tag = "2")]
    pub extension_number: i32,
}
/// The message sent by the server to answer ServerReflectionInfo method.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ServerReflectionResponse {
    #[prost(string, tag = "1")]
    pub valid_host: ::prost::alloc::string::String,
    #[prost(message, optional, tag = "2")]
    pub original_request: ::core::option::Option<ServerReflectionRequest>,
    /// The server sets one of the following fields according to the
    /// message_request in the request.
    #[prost(
        oneof = "server_reflection_response::MessageResponse",
        tags = "4, 5, 6, 7"
    )]
    pub message_response: ::core::option::Option<server_reflection_response::MessageResponse>,
}
/// Nested message and enum types in `ServerReflectionResponse`.
pub mod server_reflection_response {
    /// The server sets one of the following fields according to the
    /// message_request in the request.
    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum MessageResponse {
        /// This message is used to answer file_by_filename, file_containing_symbol,
        /// file_containing_extension requests with transitive dependencies.
        /// As the repeated label is not allowed in oneof fields, we use a
        /// FileDescriptorResponse message to encapsulate the repeated fields.
        /// The reflection service is allowed to avoid sending FileDescriptorProtos
        /// that were previously sent in response to earlier requests in the stream.
        #[prost(message, tag = "4")]
        FileDescriptorResponse(super::FileDescriptorResponse),
        /// This message is used to answer all_extension_numbers_of_type requests.
        #[prost(message, tag = "5")]
        AllExtensionNumbersResponse(super::ExtensionNumberResponse),
        /// This message is used to answer list_services requests.
        #[prost(message, tag = "6")]
        ListServicesResponse(super::ListServiceResponse),
        /// This message is used when an error occurs.
        #[prost(message, tag = "7")]
        ErrorResponse(super::ErrorResponse),
    }
}
/// Serialized FileDescriptorProto messages sent by the server answering
/// a file_by_filename, file_containing_symbol, or file_containing_extension
/// request.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct FileDescriptorResponse {
    /// Serialized FileDescriptorProto messages. We avoid taking a dependency on
    /// descriptor.proto, which uses proto2 only features, by making them opaque
    /// bytes instead.
    #[prost(bytes = "vec", repeated, tag = "1")]
    pub file_descriptor_proto: ::prost::alloc::vec::Vec<::prost::alloc::vec::Vec<u8>>,
}
/// A list of extension numbers sent by the server answering
/// all_extension_numbers_of_type request.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ExtensionNumberResponse {
    /// Full name of the base type, including the package name. The format
    /// is <package>.<type>
    #[prost(string, tag = "1")]
    pub base_type_name: ::prost::alloc::string::String,
    #[prost(int32, repeated, tag = "2")]
    pub extension_number: ::prost::alloc::vec::Vec<i32>,
}
/// A list of ServiceResponse sent by the server answering list_services request.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ListServiceResponse {
    /// The information of each service may be expanded in the future, so we use
    /// ServiceResponse message to encapsulate it.
    #[prost(message, repeated, tag = "1")]
    pub service: ::prost::alloc::vec::Vec<ServiceResponse>,
}
/// The information of a single service used by ListServiceResponse to answer
/// list_services request.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ServiceResponse {
    /// Full name of a registered service, including its package name. The format
    /// is <package>.<service>
    #[prost(string, tag = "1")]
    pub name: ::prost::alloc::string::String,
}
/// The error code and error message sent by the server when an error occurs.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ErrorResponse {
    /// This field uses the error codes defined in grpc::StatusCode.
    #[prost(int32, tag = "1")]
    pub error_code: i32,
    #[prost(string, tag = "2")]
    pub error_message: ::prost::alloc::string::String,
}
#[doc = r" Generated client implementations."]
pub mod server_reflection_client {
    #![allow(unused_variables, dead_code, missing_docs)]
    use tonic::codegen::*;
    pub struct ServerReflectionClient<T> {
        inner: tonic::client::Grpc<T>,
    }
    impl ServerReflectionClient<tonic::transport::Channel> {
        #[doc = r" Attempt to create a new client by connecting to a given endpoint."]
        pub async fn connect<D>(dst: D) -> Result<Self, tonic::transport::Error>
        where
            D: std::convert::TryInto<tonic::transport::Endpoint>,
            D::Error: Into<StdError>,
        {
            let conn = tonic::transport::Endpoint::new(dst)?.connect().await?;
            Ok(Self::new(conn))
        }
    }
    impl<T> ServerReflectionClient<T>
    where
        T: tonic::client::GrpcService<tonic::body::BoxBody>,
        T::ResponseBody: Body + HttpBody + Send + 'static,
        T::Error: Into<StdError>,
        <T::ResponseBody as HttpBody>::Error: Into<StdError> + Send,
    {
        pub fn new(inner: T) -> Self {
            let inner = tonic::client::Grpc::new(inner);
            Self { inner }
        }
        pub fn with_interceptor(inner: T, interceptor: impl Into<tonic::Interceptor>) -> Self {
            let inner = tonic::client::Grpc::with_interceptor(inner, interceptor);
            Self { inner }
        }
        #[doc = " The reflection service is structured as a bidirectional stream, ensuring"]
        #[doc = " all related requests go to a single server."]
        pub async fn server_reflection_info(
            &mut self,
            request: impl tonic::IntoStreamingRequest<Message = super::ServerReflectionRequest>,
        ) -> Result<
            tonic::Response<tonic::codec::Streaming<super::ServerReflectionResponse>>,
            tonic::Status,
        > {
            self.inner.ready().await.map_err(|e| {
                tonic::Status::new(
                    tonic::Code::Unknown,
                    format!("Service was not ready: {}", e.into()),
                )
            })?;
            let codec = tonic::codec::ProstCodec::default();
            let path = http::uri::PathAndQuery::from_static(
                "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo",
            );
            self.inner
                .streaming(request.into_streaming_request(), path, codec)
                .await
        }
    }
    impl<T: Clone> Clone for ServerReflectionClient<T> {
        fn clone(&self) -> Self {
            Self {
                inner: self.inner.clone(),
            }
        }
    }
    impl<T> std::fmt::Debug for ServerReflectionClient<T> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "ServerReflectionClient {{ ... }}")
        }
    }
}
#[doc = r" Generated server implementations."]
pub mod server_reflection_server {
    #![allow(unused_variables, dead_code, missing_docs)]
    use tonic::codegen::*;
    #[doc = "Generated trait containing gRPC methods that should be implemented for use with ServerReflectionServer."]
    #[async_trait]
    pub trait ServerReflection: Send + Sync + 'static {
        #[doc = "Server streaming response type for the ServerReflectionInfo method."]
        type ServerReflectionInfoStream: Stream<Item = Result<super::ServerReflectionResponse, tonic::Status>>
            + Send
            + Sync
            + 'static;
        #[doc = " The reflection service is structured as a bidirectional stream, ensuring"]
        #[doc = " all related requests go to a single server."]
        async fn server_reflection_info(
            &self,
            request: tonic::Request<tonic::Streaming<super::ServerReflectionRequest>>,
        ) -> Result<tonic::Response<Self::ServerReflectionInfoStream>, tonic::Status>;
    }
    #[derive(Debug)]
    pub struct ServerReflectionServer<T: ServerReflection> {
        inner: _Inner<T>,
    }
    struct _Inner<T>(Arc<T>, Option<tonic::Interceptor>);
    impl<T: ServerReflection> ServerReflectionServer<T> {
        pub fn new(inner: T) -> Self {
            let inner = Arc::new(inner);
            let inner = _Inner(inner, None);
            Self { inner }
        }
        pub fn with_interceptor(inner: T, interceptor: impl Into<tonic::Interceptor>) -> Self {
            let inner = Arc::new(inner);
            let inner = _Inner(inner, Some(interceptor.into()));
            Self { inner }
        }
    }
    impl<T, B> Service<http::Request<B>> for ServerReflectionServer<T>
    where
        T: ServerReflection,
        B: HttpBody + Send + Sync + 'static,
        B::Error: Into<StdError> + Send + 'static,
    {
        type Response = http::Response<tonic::body::BoxBody>;
        type Error = Never;
        type Future = BoxFuture<Self::Response, Self::Error>;
        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
        fn call(&mut self, req: http::Request<B>) -> Self::Future {
            let inner = self.inner.clone();
            match req.uri().path() {
                "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo" => {
                    #[allow(non_camel_case_types)]
                    struct ServerReflectionInfoSvc<T: ServerReflection>(pub Arc<T>);
                    impl<T: ServerReflection>
                        tonic::server::StreamingService<super::ServerReflectionRequest>
                        for ServerReflectionInfoSvc<T>
                    {
                        type Response = super::ServerReflectionResponse;
                        type ResponseStream = T::ServerReflectionInfoStream;
                        type Future =
                            BoxFuture<tonic::Response<Self::ResponseStream>, tonic::Status>;
                        fn call(
                            &mut self,
                            request: tonic::Request<
                                tonic::Streaming<super::ServerReflectionRequest>,
                            >,
                        ) -> Self::Future {
                            let inner = self.0.clone();
                            let fut = async move { (*inner).server_reflection_info(request).await };
                            Box::pin(fut)
                        }
                    }
                    let inner = self.inner.clone();
                    let fut = async move {
                        let interceptor = inner.1;
                        let inner = inner.0;
                        let method = ServerReflectionInfoSvc(inner);
                        let codec = tonic::codec::ProstCodec::default();
                        let mut grpc = if let Some(interceptor) = interceptor {
                            tonic::server::Grpc::with_interceptor(codec, interceptor)
                        } else {
                            tonic::server::Grpc::new(codec)
                        };
                        let res = grpc.streaming(method, req).await;
                        Ok(res)
                    };
                    Box::pin(fut)
                }
                _ => Box::pin(async move {
                    Ok(http::Response::builder()
                        .status(200)
                        .header("grpc-status", "12")
                        .header("content-type", "application/grpc")
                        .body(tonic::body::BoxBody::empty())
                        .unwrap())
                }),
            }
        }
    }
    impl<T: ServerReflection> Clone for ServerReflectionServer<T> {
        fn clone(&self) -> Self {
            let inner = self.inner.clone();
            Self { inner }
        }
    }
    impl<T: ServerReflection> Clone for _Inner<T> {
        fn clone(&self) -> Self {
            Self(self.0.clone(), self.1.clone())
        }
    }
    impl<T: std::fmt::Debug> std::fmt::Debug for _Inner<T> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{:?}", self.0)
        }
    }
    impl<T: ServerReflection> tonic::transport::NamedService for ServerReflectionServer<T> {
        const NAME: &'static str = "grpc.reflection.v1alpha.ServerReflection";
    }
}
