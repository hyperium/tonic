use std::collections::HashSet;

use super::{Attributes, Method, Service};
use crate::{generate_doc_comments, naive_snake_case, sanitize_name,
    format_method_name, format_method_path, format_service_name,
};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

/// Generate service for client.
///
/// This takes some `Service` and will generate a `TokenStream` that contains
/// a public module with the generated client.
#[deprecated(since = "0.8.3", note = "Use the CodeGenBuilder::generate_client")]
pub fn generate<T: Service>(
    service: &T,
    emit_package: bool,
    proto_path: &str,
    compile_well_known_types: bool,
    build_transport: bool,
    attributes: &Attributes,
) -> TokenStream {
    generate_internal(
        service,
        emit_package,
        proto_path,
        compile_well_known_types,
        build_transport,
        attributes,
        &HashSet::default(),
    )
}

pub(crate) fn generate_internal<T: Service>(
    service: &T,
    emit_package: bool,
    proto_path: &str,
    compile_well_known_types: bool,
    build_transport: bool,
    attributes: &Attributes,
    disable_comments: &HashSet<String>,
) -> TokenStream {
    let service_ident = quote::format_ident!("{}Client", service.name());
    let client_mod = quote::format_ident!("{}_client", naive_snake_case(service.name()));
    let methods = generate_methods(
        service,
        emit_package,
        proto_path,
        compile_well_known_types,
        disable_comments,
    );

    let connect = generate_connect(&service_ident, build_transport);

    let package = if emit_package { service.package() } else { "" };
    let service_name = format_service_name(service, emit_package);

    let service_doc = if disable_comments.contains(&service_name) {
        TokenStream::new()
    } else {
        generate_doc_comments(service.comment())
    };

    let mod_attributes = attributes.for_mod(package);
    let struct_attributes = attributes.for_struct(&service_name);

    quote! {
        /// Generated client implementations.
        #(#mod_attributes)*
        pub mod #client_mod {
            #![allow(
                unused_variables,
                dead_code,
                missing_docs,
                // will trigger if compression is disabled
                clippy::let_unit_value,
            )]
            use tonic::codegen::*;
            use tonic::codegen::http::Uri;

            #service_doc
            #(#struct_attributes)*
            #[derive(Debug, Clone)]
            pub struct #service_ident<T> {
                inner: tonic::client::Grpc<T>,
            }

            #connect

            impl<T> #service_ident<T>
            where
                T: tonic::client::GrpcService<tonic::body::BoxBody>,
                T::Error: Into<StdError>,
                T::ResponseBody: Body<Data = Bytes> + Send  + 'static,
                <T::ResponseBody as Body>::Error: Into<StdError> + Send,
            {
                pub fn new(inner: T) -> Self {
                    let inner = tonic::client::Grpc::new(inner);
                    Self { inner }
                }

                pub fn with_origin(inner: T, origin: Uri) -> Self {
                    let inner = tonic::client::Grpc::with_origin(inner, origin);
                    Self { inner }
                }

                pub fn with_interceptor<F>(inner: T, interceptor: F) -> #service_ident<InterceptedService<T, F>>
                where
                    F: tonic::service::Interceptor,
                    T::ResponseBody: Default,
                    T: tonic::codegen::Service<
                        http::Request<tonic::body::BoxBody>,
                        Response = http::Response<<T as tonic::client::GrpcService<tonic::body::BoxBody>>::ResponseBody>
                    >,
                    <T as tonic::codegen::Service<http::Request<tonic::body::BoxBody>>>::Error: Into<StdError> + Send + Sync,
                {
                    #service_ident::new(InterceptedService::new(inner, interceptor))
                }

                /// Compress requests with the given encoding.
                ///
                /// This requires the server to support it otherwise it might respond with an
                /// error.
                #[must_use]
                pub fn send_compressed(mut self, encoding: CompressionEncoding) -> Self {
                    self.inner = self.inner.send_compressed(encoding);
                    self
                }

                /// Enable decompressing responses.
                #[must_use]
                pub fn accept_compressed(mut self, encoding: CompressionEncoding) -> Self {
                    self.inner = self.inner.accept_compressed(encoding);
                    self
                }

                /// Limits the maximum size of a decoded message.
                #[must_use]
                pub fn max_decoding_message_size(mut self, limit: usize) -> Self {
                    self.inner = self.inner.max_decoding_message_size(limit);
                    self
                }

                /// Limits the maximum size of an encoded message.
                #[must_use]
                pub fn max_encoding_message_size(mut self, limit: usize) -> Self {
                    self.inner = self.inner.max_encoding_message_size(limit);
                    self
                }

                #methods
            }
        }
    }
}

#[cfg(feature = "transport")]
fn generate_connect(service_ident: &syn::Ident, enabled: bool) -> TokenStream {
    let connect_impl = quote! {
        impl #service_ident<tonic::transport::Channel> {
            /// Attempt to create a new client by connecting to a given endpoint.
            pub async fn connect<D>(dst: D) -> Result<Self, tonic::transport::Error>
            where
                D: TryInto<tonic::transport::Endpoint>,
                D::Error: Into<StdError>,
            {
                let conn = tonic::transport::Endpoint::new(dst)?.connect().await?;
                Ok(Self::new(conn))
            }
        }
    };

    if enabled {
        connect_impl
    } else {
        TokenStream::new()
    }
}

#[cfg(not(feature = "transport"))]
fn generate_connect(_service_ident: &syn::Ident, _enabled: bool) -> TokenStream {
    TokenStream::new()
}

fn generate_methods<T: Service>(
    service: &T,
    emit_package: bool,
    proto_path: &str,
    compile_well_known_types: bool,
    disable_comments: &HashSet<String>,
) -> TokenStream {
    let mut stream = TokenStream::new();

    for method in service.methods() {
        if !disable_comments.contains(&format_method_name(service, method, emit_package)) {
            stream.extend(generate_doc_comments(method.comment()));
        }

        let method = match (method.client_streaming(), method.server_streaming()) {
            (false, false) => generate_unary(
                service,
                method,
                emit_package,
                proto_path,
                compile_well_known_types,
            ),
            (false, true) => generate_server_streaming(
                service,
                method,
                emit_package,
                proto_path,
                compile_well_known_types,
            ),
            (true, false) => generate_client_streaming(
                service,
                method,
                emit_package,
                proto_path,
                compile_well_known_types,
            ),
            (true, true) => generate_streaming(
                service,
                method,
                emit_package,
                proto_path,
                compile_well_known_types,
            ),
        };

        stream.extend(method);
    }

    stream
}

fn generate_unary<T: Service>(
    service: &T,
    method: &T::Method,
    emit_package: bool,
    proto_path: &str,
    compile_well_known_types: bool,
) -> TokenStream {
    let codec_name = syn::parse_str::<syn::Path>(method.codec_path()).unwrap();
    let ident = format_ident!("{}", sanitize_name(method.name()));
    let (request, response) = method.request_response_name(proto_path, compile_well_known_types);
    let service_name = format_service_name(service, emit_package);
    let path = format_method_path(service, method, emit_package);
    let method_name = method.identifier();

    quote! {
        pub async fn #ident(
            &mut self,
            request: impl tonic::IntoRequest<#request>,
        ) -> std::result::Result<tonic::Response<#response>, tonic::Status> {
           self.inner.ready().await.map_err(|e| {
               tonic::Status::new(tonic::Code::Unknown, format!("Service was not ready: {}", e.into()))
           })?;
           let codec = #codec_name::default();
           let path = http::uri::PathAndQuery::from_static(#path);
           let mut req = request.into_request();
           req.extensions_mut().insert(GrpcMethod::new(#service_name, #method_name));
           self.inner.unary(req, path, codec).await
        }
    }
}

fn generate_server_streaming<T: Service>(
    service: &T,
    method: &T::Method,
    emit_package: bool,
    proto_path: &str,
    compile_well_known_types: bool,
) -> TokenStream {
    let codec_name = syn::parse_str::<syn::Path>(method.codec_path()).unwrap();
    let ident = format_ident!("{}", sanitize_name(method.name()));

    let (request, response) = method.request_response_name(proto_path, compile_well_known_types);
    let service_name = format_service_name(service, emit_package);
    let path = format_method_path(service, method, emit_package);
    let method_name = method.identifier();

    quote! {
        pub async fn #ident(
            &mut self,
            request: impl tonic::IntoRequest<#request>,
        ) -> std::result::Result<tonic::Response<tonic::codec::Streaming<#response>>, tonic::Status> {
            self.inner.ready().await.map_err(|e| {
                        tonic::Status::new(tonic::Code::Unknown, format!("Service was not ready: {}", e.into()))
            })?;
            let codec = #codec_name::default();
            let path = http::uri::PathAndQuery::from_static(#path);
            let mut req = request.into_request();
            req.extensions_mut().insert(GrpcMethod::new(#service_name, #method_name));
            self.inner.server_streaming(req, path, codec).await
        }
    }
}

fn generate_client_streaming<T: Service>(
    service: &T,
    method: &T::Method,
    emit_package: bool,
    proto_path: &str,
    compile_well_known_types: bool,
) -> TokenStream {
    let codec_name = syn::parse_str::<syn::Path>(method.codec_path()).unwrap();
    let ident = format_ident!("{}", sanitize_name(method.name()));

    let (request, response) = method.request_response_name(proto_path, compile_well_known_types);
    let service_name = format_service_name(service, emit_package);
    let path = format_method_path(service, method, emit_package);
    let method_name = method.identifier();

    quote! {
        pub async fn #ident(
            &mut self,
            request: impl tonic::IntoStreamingRequest<Message = #request>
        ) -> std::result::Result<tonic::Response<#response>, tonic::Status> {
            self.inner.ready().await.map_err(|e| {
                        tonic::Status::new(tonic::Code::Unknown, format!("Service was not ready: {}", e.into()))
            })?;
            let codec = #codec_name::default();
            let path = http::uri::PathAndQuery::from_static(#path);
            let mut req = request.into_streaming_request();
            req.extensions_mut().insert(GrpcMethod::new(#service_name, #method_name));
            self.inner.client_streaming(req, path, codec).await
        }
    }
}

fn generate_streaming<T: Service>(
    service: &T,
    method: &T::Method,
    emit_package: bool,
    proto_path: &str,
    compile_well_known_types: bool,
) -> TokenStream {
    let codec_name = syn::parse_str::<syn::Path>(method.codec_path()).unwrap();
    let ident = format_ident!("{}", sanitize_name(method.name()));

    let (request, response) = method.request_response_name(proto_path, compile_well_known_types);
    let service_name = format_service_name(service, emit_package);
    let path = format_method_path(service, method, emit_package);
    let method_name = method.identifier();

    quote! {
        pub async fn #ident(
            &mut self,
            request: impl tonic::IntoStreamingRequest<Message = #request>
        ) -> std::result::Result<tonic::Response<tonic::codec::Streaming<#response>>, tonic::Status> {
            self.inner.ready().await.map_err(|e| {
                        tonic::Status::new(tonic::Code::Unknown, format!("Service was not ready: {}", e.into()))
            })?;
            let codec = #codec_name::default();
            let path = http::uri::PathAndQuery::from_static(#path);
            let mut req = request.into_streaming_request();
            req.extensions_mut().insert(GrpcMethod::new(#service_name,#method_name));
            self.inner.streaming(req, path, codec).await
        }
    }
}
