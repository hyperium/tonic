/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

use std::collections::HashSet;

use super::{Attributes, Method, Service};
use crate::{
    format_method_name, format_method_path, format_service_name, generate_deprecated,
    generate_doc_comments, naive_snake_case,
};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

#[allow(clippy::too_many_arguments)]
pub(crate) fn generate_internal<T: Service>(
    service: &T,
    emit_package: bool,
    proto_path: &str,
    compile_well_known_types: bool,
    build_transport: bool,
    attributes: &Attributes,
    disable_comments: &HashSet<String>,
    tonic_path: &syn::Path,
) -> TokenStream {
    let service_ident = quote::format_ident!("{}Client", service.name());
    let client_mod = quote::format_ident!("{}_client", naive_snake_case(service.name()));
    let methods = generate_methods(
        service,
        emit_package,
        proto_path,
        compile_well_known_types,
        disable_comments,
        tonic_path,
    );

    let connect = generate_connect(&service_ident, build_transport, tonic_path);

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
                clippy::wildcard_imports,
                // will trigger if compression is disabled
                clippy::let_unit_value,
            )]
            use #tonic_path::codegen::*;
            use #tonic_path::codegen::http::Uri;

            #service_doc
            #(#struct_attributes)*
            #[derive(Debug, Clone)]
            pub struct #service_ident<T> {
                inner: #tonic_path::client::Grpc<T>,
            }

            #connect

            impl<T> #service_ident<T>
            where
                T: #tonic_path::client::GrpcService<#tonic_path::body::Body>,
                T::Error: Into<StdError>,
                T::ResponseBody: Body<Data = Bytes> + std::marker::Send  + 'static,
                <T::ResponseBody as Body>::Error: Into<StdError> + std::marker::Send,
            {
                pub fn new(inner: T) -> Self {
                    let inner = #tonic_path::client::Grpc::new(inner);
                    Self { inner }
                }

                pub fn with_origin(inner: T, origin: Uri) -> Self {
                    let inner = #tonic_path::client::Grpc::with_origin(inner, origin);
                    Self { inner }
                }

                pub fn with_interceptor<F>(inner: T, interceptor: F) -> #service_ident<InterceptedService<T, F>>
                where
                    F: #tonic_path::service::Interceptor,
                    T::ResponseBody: Default,
                    T: #tonic_path::codegen::Service<
                        http::Request<#tonic_path::body::Body>,
                        Response = http::Response<<T as #tonic_path::client::GrpcService<#tonic_path::body::Body>>::ResponseBody>
                    >,
                    <T as #tonic_path::codegen::Service<http::Request<#tonic_path::body::Body>>>::Error: Into<StdError> + std::marker::Send + std::marker::Sync,
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
                ///
                /// Default: `4MB`
                #[must_use]
                pub fn max_decoding_message_size(mut self, limit: usize) -> Self {
                    self.inner = self.inner.max_decoding_message_size(limit);
                    self
                }

                /// Limits the maximum size of an encoded message.
                ///
                /// Default: `usize::MAX`
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
fn generate_connect(
    service_ident: &syn::Ident,
    enabled: bool,
    tonic_path: &syn::Path,
) -> TokenStream {
    let connect_impl = quote! {
        impl #service_ident<#tonic_path::transport::Channel> {
            /// Attempt to create a new client by connecting to a given endpoint.
            pub async fn connect<D>(dst: D) -> Result<Self, #tonic_path::transport::Error>
            where
                D: TryInto<#tonic_path::transport::Endpoint>,
                D::Error: Into<StdError>,
            {
                let conn = #tonic_path::transport::Endpoint::new(dst)?.connect().await?;
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
fn generate_connect(
    _service_ident: &syn::Ident,
    _enabled: bool,
    _tonic_path: &syn::Path,
) -> TokenStream {
    TokenStream::new()
}

fn generate_methods<T: Service>(
    service: &T,
    emit_package: bool,
    proto_path: &str,
    compile_well_known_types: bool,
    disable_comments: &HashSet<String>,
    tonic_path: &syn::Path,
) -> TokenStream {
    let mut stream = TokenStream::new();

    for method in service.methods() {
        if !disable_comments.contains(&format_method_name(service, method, emit_package)) {
            stream.extend(generate_doc_comments(method.comment()));
        }
        if method.deprecated() {
            stream.extend(generate_deprecated());
        }

        let method = match (method.client_streaming(), method.server_streaming()) {
            (false, false) => generate_unary(
                service,
                method,
                emit_package,
                proto_path,
                compile_well_known_types,
                tonic_path,
            ),
            (false, true) => generate_server_streaming(
                service,
                method,
                emit_package,
                proto_path,
                compile_well_known_types,
                tonic_path,
            ),
            (true, false) => generate_client_streaming(
                service,
                method,
                emit_package,
                proto_path,
                compile_well_known_types,
                tonic_path,
            ),
            (true, true) => generate_streaming(
                service,
                method,
                emit_package,
                proto_path,
                compile_well_known_types,
                tonic_path,
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
    tonic_path: &syn::Path,
) -> TokenStream {
    let codec_name = syn::parse_str::<syn::Path>(method.codec_path()).unwrap();
    let ident = format_ident!("{}", method.name());
    let (request, response) = method.request_response_name(proto_path, compile_well_known_types);
    let service_name = format_service_name(service, emit_package);
    let path = format_method_path(service, method, emit_package);
    let method_name = method.identifier();

    quote! {
        pub async fn #ident(
            &mut self,
            request: impl #tonic_path::IntoRequest<#request>,
        ) -> std::result::Result<#tonic_path::Response<#response>, #tonic_path::Status> {
           self.inner.ready().await.map_err(|e| {
               #tonic_path::Status::unknown(format!("Service was not ready: {}", e.into()))
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
    tonic_path: &syn::Path,
) -> TokenStream {
    let codec_name = syn::parse_str::<syn::Path>(method.codec_path()).unwrap();
    let ident = format_ident!("{}", method.name());
    let (request, response) = method.request_response_name(proto_path, compile_well_known_types);
    let service_name = format_service_name(service, emit_package);
    let path = format_method_path(service, method, emit_package);
    let method_name = method.identifier();

    quote! {
        pub async fn #ident(
            &mut self,
            request: impl #tonic_path::IntoRequest<#request>,
        ) -> std::result::Result<#tonic_path::Response<#tonic_path::codec::Streaming<#response>>, #tonic_path::Status> {
            self.inner.ready().await.map_err(|e| {
                #tonic_path::Status::unknown(format!("Service was not ready: {}", e.into()))
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
    tonic_path: &syn::Path,
) -> TokenStream {
    let codec_name = syn::parse_str::<syn::Path>(method.codec_path()).unwrap();
    let ident = format_ident!("{}", method.name());
    let (request, response) = method.request_response_name(proto_path, compile_well_known_types);
    let service_name = format_service_name(service, emit_package);
    let path = format_method_path(service, method, emit_package);
    let method_name = method.identifier();

    quote! {
        pub async fn #ident(
            &mut self,
            request: impl #tonic_path::IntoStreamingRequest<Message = #request>
        ) -> std::result::Result<#tonic_path::Response<#response>, #tonic_path::Status> {
            self.inner.ready().await.map_err(|e| {
                #tonic_path::Status::unknown(format!("Service was not ready: {}", e.into()))
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
    tonic_path: &syn::Path,
) -> TokenStream {
    let codec_name = syn::parse_str::<syn::Path>(method.codec_path()).unwrap();
    let ident = format_ident!("{}", method.name());
    let (request, response) = method.request_response_name(proto_path, compile_well_known_types);
    let service_name = format_service_name(service, emit_package);
    let path = format_method_path(service, method, emit_package);
    let method_name = method.identifier();

    quote! {
        pub async fn #ident(
            &mut self,
            request: impl #tonic_path::IntoStreamingRequest<Message = #request>
        ) -> std::result::Result<#tonic_path::Response<#tonic_path::codec::Streaming<#response>>, #tonic_path::Status> {
            self.inner.ready().await.map_err(|e| {
                #tonic_path::Status::unknown(format!("Service was not ready: {}", e.into()))
            })?;
            let codec = #codec_name::default();
            let path = http::uri::PathAndQuery::from_static(#path);
            let mut req = request.into_streaming_request();
            req.extensions_mut().insert(GrpcMethod::new(#service_name,#method_name));
            self.inner.streaming(req, path, codec).await
        }
    }
}
