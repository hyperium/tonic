use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::pin::Pin;
use std::sync::Arc;

use prost::{DecodeError, Message};
use prost_types::{
    DescriptorProto, EnumDescriptorProto, FieldDescriptorProto, FileDescriptorProto,
    FileDescriptorSet,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt};
use tonic::{Request, Response, Status, Streaming};

use crate::proto::server_reflection_request::MessageRequest;
use crate::proto::server_reflection_response::MessageResponse;
use crate::proto::server_reflection_server::{ServerReflection, ServerReflectionServer};
use crate::proto::{
    FileDescriptorResponse, ListServiceResponse, ServerReflectionRequest, ServerReflectionResponse,
    ServiceResponse,
};

/// Represents an error in the construction of a gRPC Reflection Service.
#[derive(Debug)]
pub enum Error {
    /// An error was encountered decoding a `prost_types::FileDescriptorSet` from a buffer.
    DecodeError(prost::DecodeError),
    /// An invalid `prost_types::FileDescriptorProto` was encountered.
    InvalidFileDescriptorSet(String),
}

impl From<DecodeError> for Error {
    fn from(e: DecodeError) -> Self {
        Error::DecodeError(e)
    }
}

impl std::error::Error for Error {}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::DecodeError(_) => f.write_str("error decoding FileDescriptorSet from buffer"),
            Error::InvalidFileDescriptorSet(s) => {
                f.write_fmt(format_args!("invalid FileDescriptorSet - {}", s))
            }
        }
    }
}

/// A builder used to construct a gRPC Reflection Service.
#[derive(Debug)]
pub struct Builder<'b> {
    file_descriptor_sets: Vec<FileDescriptorSet>,
    encoded_file_descriptor_sets: Vec<&'b [u8]>,
    include_reflection_service: bool,

    service_names: Vec<String>,
    symbols: HashMap<String, Arc<FileDescriptorProto>>,
}

impl<'b> Builder<'b> {
    /// Create a new builder that can configure a gRPC Reflection Service.
    pub fn configure() -> Self {
        Builder {
            file_descriptor_sets: Vec::new(),
            encoded_file_descriptor_sets: Vec::new(),
            include_reflection_service: true,

            service_names: Vec::new(),
            symbols: HashMap::new(),
        }
    }

    /// Registers an instance of `prost_types::FileDescriptorSet` with the gRPC Reflection
    /// Service builder.
    pub fn register_file_descriptor_set(mut self, file_descriptor_set: FileDescriptorSet) -> Self {
        self.file_descriptor_sets.push(file_descriptor_set);
        self
    }

    /// Registers a byte slice containing an encoded `prost_types::FileDescriptorSet` with
    /// the gRPC Reflection Service builder.
    ///
    /// This can be called multiple times to append new descriptors to the serivce.
    pub fn register_encoded_file_descriptor_set(
        mut self,
        encoded_file_descriptor_set: &'b [u8],
    ) -> Self {
        self.encoded_file_descriptor_sets
            .push(encoded_file_descriptor_set);
        self
    }

    /// Serve the gRPC Refection Service descriptor via the Reflection Service. This is enabled
    /// by default - set `include` to false to disable.
    pub fn include_reflection_service(mut self, include: bool) -> Self {
        self.include_reflection_service = include;
        self
    }

    /// Build a gRPC Reflection Service to be served via Tonic.
    pub fn build(self) -> Result<ServerReflectionServer<impl ServerReflection>, Error> {
        let service = self.build_service()?;
        Ok(ServerReflectionServer::new(service))
    }

    /// Construct the reflection service
    fn build_service(mut self) -> Result<ReflectionService, Error> {
        if self.include_reflection_service {
            self =
                self.register_encoded_file_descriptor_set(crate::proto::REFLECTION_DESCRIPTOR_SET);
        }

        for encoded in &self.encoded_file_descriptor_sets {
            let decoded = FileDescriptorSet::decode(*encoded)?;
            self.file_descriptor_sets.push(decoded);
        }

        let all_fds = self.file_descriptor_sets.clone();
        let mut files: HashMap<String, Arc<FileDescriptorProto>> = HashMap::new();

        for fds in all_fds {
            for fd in fds.file {
                let name = match fd.name.clone() {
                    None => {
                        return Err(Error::InvalidFileDescriptorSet("missing name".to_string()));
                    }
                    Some(n) => n,
                };

                if files.contains_key(&name) {
                    continue;
                }

                let fd = Arc::new(fd);
                files.insert(name, fd.clone());

                self.process_file(fd)?;
            }
        }

        let service_names = self
            .service_names
            .iter()
            .map(|name| ServiceResponse { name: name.clone() })
            .collect();

        Ok(ReflectionService {
            state: Arc::new(State {
                service_names,
                files,
                symbols: self.symbols,
            }),
        })
    }

    fn process_file(&mut self, fd: Arc<FileDescriptorProto>) -> Result<(), Error> {
        let prefix = &fd.package.clone().unwrap_or_default();

        for msg in &fd.message_type {
            self.process_message(fd.clone(), &prefix, msg)?;
        }

        for en in &fd.enum_type {
            self.process_enum(fd.clone(), &prefix, en)?;
        }

        for service in &fd.service {
            let service_name = extract_name(&prefix, "service", service.name.as_ref())?;
            self.service_names.push(service_name.clone());
            self.symbols.insert(service_name.clone(), fd.clone());

            for method in &service.method {
                let method_name = extract_name(&service_name, "method", method.name.as_ref())?;
                self.symbols.insert(method_name, fd.clone());
            }
        }

        Ok(())
    }

    fn process_message(
        &mut self,
        fd: Arc<FileDescriptorProto>,
        prefix: &str,
        msg: &DescriptorProto,
    ) -> Result<(), Error> {
        let message_name = extract_name(prefix, "message", msg.name.as_ref())?;
        self.symbols.insert(message_name.clone(), fd.clone());

        for nested in &msg.nested_type {
            self.process_message(fd.clone(), &message_name, nested)?;
        }

        for en in &msg.enum_type {
            self.process_enum(fd.clone(), &message_name, en)?;
        }

        for field in &msg.field {
            self.process_field(fd.clone(), &message_name, field)?;
        }

        for oneof in &msg.oneof_decl {
            let oneof_name = extract_name(&message_name, "oneof", oneof.name.as_ref())?;
            self.symbols.insert(oneof_name, fd.clone());
        }

        Ok(())
    }

    fn process_enum(
        &mut self,
        fd: Arc<FileDescriptorProto>,
        prefix: &str,
        en: &EnumDescriptorProto,
    ) -> Result<(), Error> {
        let enum_name = extract_name(prefix, "enum", en.name.as_ref())?;
        self.symbols.insert(enum_name.clone(), fd.clone());

        for value in &en.value {
            let value_name = extract_name(&enum_name, "enum value", value.name.as_ref())?;
            self.symbols.insert(value_name, fd.clone());
        }

        Ok(())
    }

    fn process_field(
        &mut self,
        fd: Arc<FileDescriptorProto>,
        prefix: &str,
        field: &FieldDescriptorProto,
    ) -> Result<(), Error> {
        let field_name = extract_name(prefix, "field", field.name.as_ref())?;
        self.symbols.insert(field_name, fd.clone());
        Ok(())
    }
}

fn extract_name(
    prefix: &str,
    name_type: &str,
    maybe_name: Option<&String>,
) -> Result<String, Error> {
    match maybe_name {
        None => Err(Error::InvalidFileDescriptorSet(format!(
            "missing {} name",
            name_type
        ))),
        Some(name) => {
            if prefix.is_empty() {
                Ok(name.to_string())
            } else {
                Ok(format!("{}.{}", prefix, name))
            }
        }
    }
}

#[derive(Debug)]
struct State {
    service_names: Vec<ServiceResponse>,
    files: HashMap<String, Arc<FileDescriptorProto>>,
    symbols: HashMap<String, Arc<FileDescriptorProto>>,
}

impl State {
    fn list_services(&self) -> MessageResponse {
        MessageResponse::ListServicesResponse(ListServiceResponse {
            service: self.service_names.clone(),
        })
    }

    fn symbol_by_name(&self, symbol: &str) -> Result<MessageResponse, Status> {
        match self.symbols.get(symbol) {
            None => Err(Status::not_found(format!("symbol '{}' not found", symbol))),
            Some(fd) => {
                let mut encoded_fd = Vec::new();
                if let Err(_) = fd.clone().encode(&mut encoded_fd) {
                    return Err(Status::internal("encoding error"));
                };

                Ok(MessageResponse::FileDescriptorResponse(
                    FileDescriptorResponse {
                        file_descriptor_proto: vec![encoded_fd],
                    },
                ))
            }
        }
    }

    fn file_by_filename(&self, filename: &str) -> Result<MessageResponse, Status> {
        match self.files.get(filename) {
            None => Err(Status::not_found(format!("file '{}' not found", filename))),
            Some(fd) => {
                let mut encoded_fd = Vec::new();
                if let Err(_) = fd.clone().encode(&mut encoded_fd) {
                    return Err(Status::internal("encoding error"));
                }

                Ok(MessageResponse::FileDescriptorResponse(
                    FileDescriptorResponse {
                        file_descriptor_proto: vec![encoded_fd],
                    },
                ))
            }
        }
    }
}

#[derive(Debug)]
struct ReflectionService {
    state: Arc<State>,
}

#[tonic::async_trait]
impl ServerReflection for ReflectionService {
    type ServerReflectionInfoStream = Pin<
        Box<dyn Stream<Item = Result<ServerReflectionResponse, Status>> + Send + Sync + 'static>,
    >;

    async fn server_reflection_info(
        &self,
        req: Request<Streaming<ServerReflectionRequest>>,
    ) -> Result<Response<Self::ServerReflectionInfoStream>, Status> {
        let mut req_rx = req.into_inner();
        let (resp_tx, resp_rx) = mpsc::channel::<Result<ServerReflectionResponse, Status>>(1);

        let state = self.state.clone();

        tokio::spawn(async move {
            while let Some(req) = req_rx.next().await {
                let req = match req {
                    Ok(req) => req,
                    Err(_) => {
                        return;
                    }
                };

                let resp_msg = match req.message_request.clone() {
                    None => Err(Status::invalid_argument("invalid MessageRequest")),
                    Some(msg) => match msg {
                        MessageRequest::FileByFilename(s) => state.file_by_filename(&s),
                        MessageRequest::FileContainingSymbol(s) => state.symbol_by_name(&s),
                        MessageRequest::FileContainingExtension(_) => {
                            Err(Status::unimplemented("extensions are not supported"))
                        }
                        MessageRequest::AllExtensionNumbersOfType(_) => {
                            Err(Status::unimplemented("extensions are not supported"))
                        }
                        MessageRequest::ListServices(_) => Ok(state.list_services()),
                    },
                };

                match resp_msg {
                    Ok(resp_msg) => {
                        let resp = ServerReflectionResponse {
                            valid_host: req.host.clone(),
                            original_request: Some(req.clone()),
                            message_response: Some(resp_msg),
                        };
                        resp_tx.send(Ok(resp)).await.expect("send");
                    }
                    Err(status) => {
                        resp_tx.send(Err(status)).await.expect("send");
                        return;
                    }
                }
            }
        });

        Ok(Response::new(Box::pin(ReceiverStream::new(resp_rx))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use prost::bytes::Bytes;
    use tonic::codec::{Codec, ProstCodec};
    use tonic::IntoStreamingRequest;

    /// Encode a message as byte buffer
    ///
    /// Implementation cribbed from https://github.com/hyperium/tonic/issues/462#issuecomment-751887539
    ///
    /// FIXME(ergo): This function is a bit arcane and should be replaced once we have a better
    ///     story around testing Streaming APIs
    fn message_to_stream(msg: impl Message) -> Result<Bytes, prost::EncodeError> {
        use bytes::{BufMut, BytesMut};
        let mut buf = BytesMut::new();
        // See below comment on spec.
        use std::mem::size_of;
        const PREFIX_BYTES: usize = size_of::<u8>() + size_of::<u32>();
        for _ in 0..PREFIX_BYTES {
            // Advance our buffer first.
            // We will backfill it once we know the size of the message.
            buf.put_u8(0);
        }

        msg.encode(&mut buf)?;
        let len = buf.len() - PREFIX_BYTES;
        {
            let mut buf = &mut buf[0..PREFIX_BYTES];
            // See: https://github.com/grpc/grpc/blob/master/doc/PROTOCOL-HTTP2.md#:~:text=Compressed-Flag
            // for more details on spec.
            // Compressed-Flag -> 0 / 1 # encoded as 1 byte unsigned integer.
            buf.put_u8(0);
            // Message-Length -> {length of Message} # encoded as 4 byte unsigned integer (big endian).
            buf.put_u32(len as u32);
            // Message -> *{binary octet}.
        }

        Ok(buf.freeze())
    }

    /// Dispatch a `ReflectionService::server_reflection_info` request
    async fn dispatch_request(
        service: &ReflectionService,
        req: &ServerReflectionRequest,
    ) -> Result<ServerReflectionResponse, Status> {
        let req_as_bytes = message_to_stream(req.clone()).unwrap();

        // create the codec
        let mut codec: ProstCodec<ServerReflectionRequest, ServerReflectionRequest> =
            ProstCodec::default();
        // wrap the codec in a `Streaming` obj
        let streaming =
            Streaming::new_request(codec.decoder(), tonic::transport::Body::from(req_as_bytes))
                .into_streaming_request();

        // dispatch the request
        let mut stream = service
            .server_reflection_info(streaming)
            .await
            .expect("Failed to perform request")
            .into_inner();

        stream.next().await.expect("Stream is missing response")
    }

    /// Build the ReflectionService w/ the provided descriptor_sets
    fn build_service(descriptor_sets: &[&[u8]]) -> ReflectionService {
        descriptor_sets
            .iter()
            .fold(
                Builder::configure().include_reflection_service(false),
                |builder, ds| builder.register_encoded_file_descriptor_set(ds),
            )
            .build_service()
            .unwrap()
    }

    #[tokio::test]
    async fn test_list_with_disabled_builtins() {
        // test builtins
        let service = build_service(&[]);

        let request = ServerReflectionRequest {
            message_request: Some(MessageRequest::ListServices(String::new())),
            ..Default::default()
        };

        let resp = dispatch_request(&service, &request).await.unwrap();

        let service_responses: Vec<ServiceResponse> = match resp.message_response.unwrap() {
            MessageResponse::ListServicesResponse(ListServiceResponse { service }) => service,
            other => panic!("Invalid response type {:?}", other),
        };

        assert_eq!(service_responses.len(), 0);
    }

    #[tokio::test]
    async fn test_list_with_reflection() {
        let service = build_service(&[crate::proto::REFLECTION_DESCRIPTOR_SET]);

        let resp = dispatch_request(
            &service,
            &ServerReflectionRequest {
                message_request: Some(MessageRequest::ListServices(Default::default())),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let service_responses: Vec<ServiceResponse> = match resp.message_response.unwrap() {
            MessageResponse::ListServicesResponse(ListServiceResponse { service }) => service,
            other => panic!("Unexpected message_response {:?}", other),
        };

        assert_eq!(service_responses.len(), 1);
        assert_eq!(
            service_responses[0].name.as_str(),
            "grpc.reflection.v1alpha.ServerReflection"
        );
    }

    #[tokio::test]
    async fn test_file_containing_symbol() {
        let service = build_service(&[crate::proto::REFLECTION_DESCRIPTOR_SET]);

        let symbol_name = "grpc.reflection.v1alpha.ListServiceResponse";
        let resp = dispatch_request(
            &service,
            &ServerReflectionRequest {
                message_request: Some(MessageRequest::FileContainingSymbol(
                    symbol_name.to_string(),
                )),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let protos = match resp.message_response.unwrap() {
            MessageResponse::FileDescriptorResponse(FileDescriptorResponse {
                file_descriptor_proto,
            }) => file_descriptor_proto,
            other => panic!("Unexpected message_response {:?}", other),
        };

        assert_eq!(protos.len(), 1);

        let expected_proto = service.state.symbols.get(symbol_name).map(|fd| {
            let mut encoded = Vec::new();
            fd.encode(&mut encoded).unwrap();
            encoded
        });
        assert_eq!(protos.iter().next(), expected_proto.as_ref())
    }

    #[tokio::test]
    async fn test_file_by_filename() {
        let service = build_service(&[crate::proto::REFLECTION_DESCRIPTOR_SET]);

        let filename = "reflection.proto";

        let resp = dispatch_request(
            &service,
            &ServerReflectionRequest {
                message_request: Some(MessageRequest::FileByFilename(filename.to_string())),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let protos = match resp.message_response.unwrap() {
            MessageResponse::FileDescriptorResponse(FileDescriptorResponse {
                file_descriptor_proto,
            }) => file_descriptor_proto,
            other => panic!("Unexpected message_response {:?}", other),
        };

        assert_eq!(protos.len(), 1);

        let expected_proto = service.state.files.get(filename).map(|fd| {
            let mut encoded = Vec::new();
            fd.encode(&mut encoded).unwrap();
            encoded
        });

        assert_eq!(protos.iter().next(), expected_proto.as_ref())
    }

    #[tokio::test]
    async fn test_unimplemented() {
        let service = build_service(&[]);
        let reqs = [
            ServerReflectionRequest {
                message_request: Some(MessageRequest::FileContainingExtension(Default::default())),
                ..Default::default()
            },
            ServerReflectionRequest {
                message_request: Some(
                    MessageRequest::AllExtensionNumbersOfType(Default::default()),
                ),
                ..Default::default()
            },
        ];

        for req in &reqs {
            match dispatch_request(&service, req).await {
                Ok(_) => panic!("Should be error"),
                Err(status) if status.code() == tonic::Code::Unimplemented => {}
                Err(other) => panic!(
                    "Expected status `Unimplemented`. Received --  `{:?}`",
                    other
                ),
            }
        }
    }
}
