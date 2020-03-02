use super::schema::{Context, Method, Service};
use crate::{generate_doc_comments, naive_snake_case};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

/// Generate service for client
pub fn generate<'a, T: Service<'a>>(service: &'a T, context: &T::Context) -> TokenStream {
    let service_ident = quote::format_ident!("{}Client", service.name());
    let client_mod = quote::format_ident!("{}_client", naive_snake_case(&service.name()));
    let methods = generate_methods(service, context);

    let connect = generate_connect(&service_ident);
    let service_doc = generate_doc_comments(service.comment());

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

fn generate_methods<'a, T: Service<'a>>(service: &'a T, context: &T::Context) -> TokenStream {
    let mut stream = TokenStream::new();

    for method in service.methods() {
        use super::schema::Commentable;

        let path = format!(
            "/{}.{}/{}",
            service.package(),
            service.identifier(),
            method.identifier()
        );

        stream.extend(generate_doc_comments(method.comment()));

        let method = match (method.client_streaming(), method.server_streaming()) {
            (false, false) => generate_unary(method, &context, path),
            (false, true) => generate_server_streaming(method, &context, path),
            (true, false) => generate_client_streaming(method, &context, path),
            (true, true) => generate_streaming(method, &context, path),
        };

        stream.extend(method);
    }

    stream
}

fn generate_unary<'a, T: Method<'a>>(
    method: &T,
    context: &T::Context,
    path: String,
) -> TokenStream {
    let codec_name = syn::parse_str::<syn::Path>(context.codec_name()).unwrap();
    let ident = format_ident!("{}", method.name());
    let (request, response) = method.request_response_name(context);

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

fn generate_server_streaming<'a, T: Method<'a>>(
    method: &T,
    context: &T::Context,
    path: String,
) -> TokenStream {
    let codec_name = syn::parse_str::<syn::Path>(context.codec_name()).unwrap();
    let ident = format_ident!("{}", method.name());

    let (request, response) = method.request_response_name(context);

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

fn generate_client_streaming<'a, T: Method<'a>>(
    method: &T,
    context: &T::Context,
    path: String,
) -> TokenStream {
    let codec_name = syn::parse_str::<syn::Path>(context.codec_name()).unwrap();
    let ident = format_ident!("{}", method.name());

    let (request, response) = method.request_response_name(context);

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

fn generate_streaming<'a, T: Method<'a>>(
    method: &T,
    context: &T::Context,
    path: String,
) -> TokenStream {
    let codec_name = syn::parse_str::<syn::Path>(context.codec_name()).unwrap();
    let ident = format_ident!("{}", method.name());

    let (request, response) = method.request_response_name(context);

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
