use crate::pb::v1::server_reflection_server::{ServerReflection, ServerReflectionServer};

use crate::pb::v1::server_reflection_request::MessageRequest;
use crate::pb::v1::server_reflection_response::MessageResponse;
use crate::pb::v1::{
    ExtensionNumberResponse, FileDescriptorResponse, ListServiceResponse, ServerReflectionRequest,
    ServerReflectionResponse, ServiceResponse,
};
use prost::Message;
use prost_types::{FileDescriptorProto, FileDescriptorSet};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tonic::{Request, Response, Status, Streaming};

use crate::server::Error;

/// A builder used to construct a gRPC Reflection Service.
#[derive(Debug)]
pub struct Builder<'b> {
    file_descriptor_sets: Vec<FileDescriptorSet>,
    encoded_file_descriptor_sets: Vec<&'b [u8]>,
    include_reflection_service: bool,

    service_names: Vec<String>,
    use_all_service_names: bool,
}

impl<'b> Builder<'b> {
    /// Create a new builder that can configure a gRPC Reflection Service.
    pub fn configure() -> Self {
        Builder {
            file_descriptor_sets: Vec::new(),
            encoded_file_descriptor_sets: Vec::new(),
            include_reflection_service: true,

            service_names: Vec::new(),
            use_all_service_names: true,
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
    pub fn register_encoded_file_descriptor_set(
        mut self,
        encoded_file_descriptor_set: &'b [u8],
    ) -> Self {
        self.encoded_file_descriptor_sets
            .push(encoded_file_descriptor_set);
        self
    }

    /// Serve the gRPC Reflection Service descriptor via the Reflection Service. This is enabled
    /// by default - set `include` to false to disable.
    pub fn include_reflection_service(mut self, include: bool) -> Self {
        self.include_reflection_service = include;
        self
    }

    /// Advertise a fully-qualified gRPC service name.
    ///
    /// If not called, then all services present in the registered file descriptor sets
    /// will be advertised.
    pub fn with_service_name(mut self, name: impl Into<String>) -> Self {
        self.use_all_service_names = false;
        self.service_names.push(name.into());
        self
    }

    /// Build a gRPC Reflection Service to be served via Tonic.
    pub fn build(mut self) -> Result<ServerReflectionServer<impl ServerReflection>, Error> {
        if self.include_reflection_service {
            self = self.register_encoded_file_descriptor_set(crate::pb::v1::FILE_DESCRIPTOR_SET);
        }

        let info = crate::server::parser::DescriptorParser::process(
            self.encoded_file_descriptor_sets,
            self.file_descriptor_sets,
        )?;

        let service_names = if self.use_all_service_names {
            &info.service_names
        } else {
            &self.service_names
        };

        Ok(ServerReflectionServer::new(ReflectionService::new(
            Arc::new(ReflectionServiceState::new(
                service_names,
                info.files,
                info.symbols,
            )),
        )))
    }
}

#[derive(Debug)]
struct ReflectionServiceState {
    service_names: Vec<ServiceResponse>,
    files: HashMap<String, Arc<FileDescriptorProto>>,
    symbols: HashMap<String, Arc<FileDescriptorProto>>,
}

impl ReflectionServiceState {
    fn new(
        service_names: &Vec<String>,
        files: HashMap<String, Arc<FileDescriptorProto>>,
        symbols: HashMap<String, Arc<FileDescriptorProto>>,
    ) -> Self {
        let service_names = service_names
            .iter()
            .map(|name| ServiceResponse { name: name.clone() })
            .collect();

        ReflectionServiceState {
            service_names,
            files,
            symbols,
        }
    }

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
                if fd.clone().encode(&mut encoded_fd).is_err() {
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
                if fd.clone().encode(&mut encoded_fd).is_err() {
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
    state: Arc<ReflectionServiceState>,
}

impl ReflectionService {
    fn new(state: Arc<ReflectionServiceState>) -> Self {
        ReflectionService { state }
    }
}

#[tonic::async_trait]
impl ServerReflection for ReflectionService {
    type ServerReflectionInfoStream = ReceiverStream<Result<ServerReflectionResponse, Status>>;

    async fn server_reflection_info(
        &self,
        req: Request<Streaming<ServerReflectionRequest>>,
    ) -> Result<Response<Self::ServerReflectionInfoStream>, Status> {
        let mut req_rx = req.into_inner();
        let (resp_tx, resp_rx) = mpsc::channel::<Result<ServerReflectionResponse, Status>>(1);

        let state = self.state.clone();

        tokio::spawn(async move {
            while let Some(req) = req_rx.next().await {
                let Ok(req) = req else {
                    return;
                };

                let resp_msg = match req.message_request.clone() {
                    None => Err(Status::invalid_argument("invalid MessageRequest")),
                    Some(msg) => match msg {
                        MessageRequest::FileByFilename(s) => state.file_by_filename(&s),
                        MessageRequest::FileContainingSymbol(s) => state.symbol_by_name(&s),
                        MessageRequest::FileContainingExtension(_) => {
                            Err(Status::not_found("extensions are not supported"))
                        }
                        MessageRequest::AllExtensionNumbersOfType(_) => {
                            // NOTE: Workaround. Some grpc clients (e.g. grpcurl) expect this method not to fail.
                            // https://github.com/hyperium/tonic/issues/1077
                            Ok(MessageResponse::AllExtensionNumbersResponse(
                                ExtensionNumberResponse::default(),
                            ))
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

        Ok(Response::new(ReceiverStream::new(resp_rx)))
    }
}
