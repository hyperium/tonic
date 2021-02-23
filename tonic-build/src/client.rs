use super::{Method, Service};
use crate::{generate_doc_comments, naive_snake_case};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

/// Generate service for client.
///
/// This takes some `Service` and will generate a `TokenStream` that contains
/// a public module with the generated client.
pub fn generate<T: Service>(
    service: &T,
    emit_package: bool,
    proto_path: &str,
    compile_well_known_types: bool,
) -> TokenStream {
    let service_ident = quote::format_ident!("{}Client", service.name());
    let client_mod = quote::format_ident!("{}_client", naive_snake_case(&service.name()));
    let methods = generate_methods(service, emit_package, proto_path, compile_well_known_types);

    let connect = generate_connect(&service_ident);
    let service_doc = generate_doc_comments(service.comment());

    let struct_debug = format!("{} {{{{ ... }}}}", &service_ident);

    quote! {
        /// Generated client implementations.
        pub mod #client_mod {
            #![allow(unused_variables, dead_code, missing_docs)]
            use tonic::codegen::*;

            #service_doc
            pub struct #service_ident<T> {
                inner: tonic::client::Grpc<T>,
            }

            #connect

            impl<T> #service_ident<T>
            where T: tonic::client::GrpcService<tonic::body::BoxBody>,
                  T::ResponseBody: Body + HttpBody + Send + 'static,
                  T::Error: Into<StdError>,
                  <T::ResponseBody as HttpBody>::Error: Into<StdError> + Send, {
                pub fn new(inner: T) -> Self {
                    let inner = tonic::client::Grpc::new(inner);
                    Self { inner }
                }

                pub fn with_interceptor(inner: T, interceptor: impl Into<tonic::Interceptor>) -> Self {
                    let inner = tonic::client::Grpc::with_interceptor(inner, interceptor);
                    Self { inner }
                }

                #methods
            }

            impl<T: Clone> Clone for #service_ident<T> {
                fn clone(&self) -> Self {
                    Self {
                        inner: self.inner.clone(),
                    }
                }
            }

            impl<T> std::fmt::Debug for #service_ident<T> {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                   write!(f, #struct_debug)
                }
            }
        }
    }
}

#[cfg(feature = "transport")]
fn generate_connect(service_ident: &syn::Ident) -> TokenStream {
    quote! {
        impl #service_ident<tonic::transport::Channel> {
            /// Attempt to create a new client by connecting to a given endpoint.
            pub async fn connect<D>(dst: D) -> Result<Self, tonic::transport::Error>
            where
                D: std::convert::TryInto<tonic::transport::Endpoint>,
                D::Error: Into<StdError>,
            {
                let conn = tonic::transport::Endpoint::new(dst)?.connect().await?;
                Ok(Self::new(conn))
            }
        }
    }
}

#[cfg(not(feature = "transport"))]
fn generate_connect(_service_ident: &syn::Ident) -> TokenStream {
    TokenStream::new()
}

fn generate_methods<T: Service>(
    service: &T,
    emit_package: bool,
    proto_path: &str,
    compile_well_known_types: bool,
) -> TokenStream {
    let mut stream = TokenStream::new();
    let package = if emit_package { service.package() } else { "" };

    for method in service.methods() {
        let path = format!(
            "/{}{}{}/{}",
            package,
            if package.is_empty() { "" } else { "." },
            service.identifier(),
            method.identifier()
        );

        stream.extend(generate_doc_comments(method.comment()));

        let method = match (method.client_streaming(), method.server_streaming()) {
            (false, false) => generate_unary(method, proto_path, compile_well_known_types, path),
            (false, true) => {
                generate_server_streaming(method, proto_path, compile_well_known_types, path)
            }
            (true, false) => {
                generate_client_streaming(method, proto_path, compile_well_known_types, path)
            }
            (true, true) => generate_streaming(method, proto_path, compile_well_known_types, path),
        };

        stream.extend(method);
    }

    stream
}

fn generate_unary<T: Method>(
    method: &T,
    proto_path: &str,
    compile_well_known_types: bool,
    path: String,
) -> TokenStream {
    let codec_name = syn::parse_str::<syn::Path>(T::CODEC_PATH).unwrap();
    let ident = format_ident!("{}", method.name());
    let (request, response) = method.request_response_name(proto_path, compile_well_known_types);

    quote! {
        pub async fn #ident(
            &mut self,
            request: impl tonic::IntoRequest<#request>,
        ) -> Result<tonic::Response<#response>, tonic::Status> {
            self.inner.ready().await.map_err(|e| {
                        tonic::Status::new(tonic::Code::Unknown, format!("Service was not ready: {}", e.into()))
            })?;
            let codec = #codec_name::default();
           let path = http::uri::PathAndQuery::from_static(#path);
           self.inner.unary(request.into_request(), path, codec).await
        }
    }
}

fn generate_server_streaming<T: Method>(
    method: &T,
    proto_path: &str,
    compile_well_known_types: bool,
    path: String,
) -> TokenStream {
    let codec_name = syn::parse_str::<syn::Path>(T::CODEC_PATH).unwrap();
    let ident = format_ident!("{}", method.name());

    let (request, response) = method.request_response_name(proto_path, compile_well_known_types);

    quote! {
        pub async fn #ident(
            &mut self,
            request: impl tonic::IntoRequest<#request>,
        ) -> Result<tonic::Response<tonic::codec::Streaming<#response>>, tonic::Status> {
            self.inner.ready().await.map_err(|e| {
                        tonic::Status::new(tonic::Code::Unknown, format!("Service was not ready: {}", e.into()))
            })?;
            let codec = #codec_name::default();
           let path = http::uri::PathAndQuery::from_static(#path);
           self.inner.server_streaming(request.into_request(), path, codec).await
        }
    }
}

fn generate_client_streaming<T: Method>(
    method: &T,
    proto_path: &str,
    compile_well_known_types: bool,
    path: String,
) -> TokenStream {
    let codec_name = syn::parse_str::<syn::Path>(T::CODEC_PATH).unwrap();
    let ident = format_ident!("{}", method.name());

    let (request, response) = method.request_response_name(proto_path, compile_well_known_types);

    quote! {
        pub async fn #ident(
            &mut self,
            request: impl tonic::IntoStreamingRequest<Message = #request>
        ) -> Result<tonic::Response<#response>, tonic::Status> {
            self.inner.ready().await.map_err(|e| {
                        tonic::Status::new(tonic::Code::Unknown, format!("Service was not ready: {}", e.into()))
            })?;
            let codec = #codec_name::default();
            let path = http::uri::PathAndQuery::from_static(#path);
            self.inner.client_streaming(request.into_streaming_request(), path, codec).await
        }
    }
}

fn generate_streaming<T: Method>(
    method: &T,
    proto_path: &str,
    compile_well_known_types: bool,
    path: String,
) -> TokenStream {
    let codec_name = syn::parse_str::<syn::Path>(T::CODEC_PATH).unwrap();
    let ident = format_ident!("{}", method.name());

    let (request, response) = method.request_response_name(proto_path, compile_well_known_types);

    quote! {
        pub async fn #ident(
            &mut self,
            request: impl tonic::IntoStreamingRequest<Message = #request>
        ) -> Result<tonic::Response<tonic::codec::Streaming<#response>>, tonic::Status> {
            self.inner.ready().await.map_err(|e| {
                        tonic::Status::new(tonic::Code::Unknown, format!("Service was not ready: {}", e.into()))
            })?;
            let codec = #codec_name::default();
           let path = http::uri::PathAndQuery::from_static(#path);
           self.inner.streaming(request.into_streaming_request(), path, codec).await
        }
    }
}
