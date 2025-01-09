use std::{fmt, sync::Arc};

use tokio::sync::mpsc;
use tokio_stream::{wrappers::ReceiverStream, Stream, StreamExt};
use tonic::{Request, Response, Status, Streaming};

use super::ReflectionServiceState;
use crate::pb::v1::server_reflection_request::MessageRequest;
use crate::pb::v1::server_reflection_response::MessageResponse;
pub use crate::pb::v1::server_reflection_server::{ServerReflection, ServerReflectionServer};
use crate::pb::v1::{
    ExtensionNumberResponse, FileDescriptorResponse, ListServiceResponse, ServerReflectionRequest,
    ServerReflectionResponse, ServiceResponse,
};

/// An implementation for `ServerReflection`.
#[derive(Debug)]
pub struct ReflectionService {
    state: Arc<ReflectionServiceState>,
}

#[tonic::async_trait]
impl ServerReflection for ReflectionService {
    type ServerReflectionInfoStream = ServerReflectionInfoStream;

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
                        MessageRequest::FileByFilename(s) => state.file_by_filename(&s).map(|fd| {
                            MessageResponse::FileDescriptorResponse(FileDescriptorResponse {
                                file_descriptor_proto: vec![fd],
                            })
                        }),
                        MessageRequest::FileContainingSymbol(s) => {
                            state.symbol_by_name(&s).map(|fd| {
                                MessageResponse::FileDescriptorResponse(FileDescriptorResponse {
                                    file_descriptor_proto: vec![fd],
                                })
                            })
                        }
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
                        MessageRequest::ListServices(_) => {
                            Ok(MessageResponse::ListServicesResponse(ListServiceResponse {
                                service: state
                                    .list_services()
                                    .iter()
                                    .map(|s| ServiceResponse { name: s.clone() })
                                    .collect(),
                            }))
                        }
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

        Ok(Response::new(ServerReflectionInfoStream(
            ReceiverStream::new(resp_rx),
        )))
    }
}

impl From<ReflectionServiceState> for ReflectionService {
    fn from(state: ReflectionServiceState) -> Self {
        Self {
            state: Arc::new(state),
        }
    }
}

/// A response stream.
pub struct ServerReflectionInfoStream(ReceiverStream<Result<ServerReflectionResponse, Status>>);

impl Stream for ServerReflectionInfoStream {
    type Item = Result<ServerReflectionResponse, Status>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::pin::Pin::new(&mut self.0).poll_next(cx)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl fmt::Debug for ServerReflectionInfoStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ServerReflectionInfoStream").finish()
    }
}
