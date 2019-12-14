use crate::{generate_doc_comment, generate_doc_comments, naive_snake_case};
use proc_macro2::{Span, TokenStream};
use prost_build::{Method, Service};
use quote::quote;
use syn::{Ident, Lit, LitStr};

pub(crate) fn generate(service: &Service, proto_path: &str) -> TokenStream {
    let methods = generate_methods(&service, proto_path);

    let server_service = quote::format_ident!("{}Server", service.name);
    let server_trait = quote::format_ident!("{}", service.name);
    let server_mod = quote::format_ident!("{}_server", naive_snake_case(&service.name));
    let generated_trait = generate_trait(service, proto_path, server_trait.clone());
    let service_doc = generate_doc_comments(&service.comments.leading);

    // Transport based implementations
    let path = format!("{}.{}", service.package, service.proto_name);
    let transport = generate_transport(&server_service, &server_trait, &path);

    quote! {
        /// Generated server implementations.
        pub mod #server_mod {
            #![allow(unused_variables, dead_code, missing_docs)]
            use tonic::codegen::*;

            #generated_trait

            #service_doc
            #[derive(Debug)]
            #[doc(hidden)]
            pub struct #server_service<T: #server_trait> {
                inner: Arc<T>,
            }

            impl<T: #server_trait> #server_service<T> {
                pub fn new(inner: T) -> Self {
                    let inner = Arc::new(inner);
                    Self { inner }
                }
            }

            impl<T: #server_trait> Service<http::Request<HyperBody>> for #server_service<T> {
                type Response = http::Response<tonic::body::BoxBody>;
                type Error = Never;
                type Future = BoxFuture<Self::Response, Self::Error>;

                fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
                    Poll::Ready(Ok(()))
                }

                fn call(&mut self, req: http::Request<HyperBody>) -> Self::Future {
                    let inner = self.inner.clone();

                    match req.uri().path() {
                        #methods

                        _ => Box::pin(async move {
                            Ok(http::Response::builder()
                               .status(200)
                               .header("grpc-status", "12")
                               .body(tonic::body::BoxBody::empty())
                               .unwrap())
                        }),
                    }
                }
            }

            impl<T: #server_trait> Clone for #server_service<T> {
                fn clone(&self) -> Self {
                    let inner = self.inner.clone();
                    Self { inner }
                }
            }

            #transport
        }
    }
}

fn generate_trait(service: &Service, proto_path: &str, server_trait: Ident) -> TokenStream {
    let methods = generate_trait_methods(service, proto_path);
    let trait_doc = generate_doc_comment(&format!(
        "Generated trait containing gRPC methods that should be implemented for use with {}Server.",
        service.name
    ));

    quote! {
        #trait_doc
        #[async_trait]
        pub trait #server_trait : Send + Sync + 'static {
            #methods
        }
    }
}

fn generate_trait_methods(service: &Service, proto_path: &str) -> TokenStream {
    let mut stream = TokenStream::new();

    for method in &service.methods {
        let name = quote::format_ident!("{}", method.name);

        let (req_message, res_message) = crate::replace_wellknown(proto_path, &method);

        let method_doc = generate_doc_comments(&method.comments.leading);

        let method = match (method.client_streaming, method.server_streaming) {
            (false, false) => {
                quote! {
                    #method_doc
                    async fn #name(&self, request: tonic::Request<#req_message>)
                        -> Result<tonic::Response<#res_message>, tonic::Status> {
                            Err(tonic::Status::unimplemented("Not yet implemented"))
                        }
                }
            }
            (true, false) => {
                quote! {
                    #method_doc
                    async fn #name(&self, request: tonic::Request<tonic::Streaming<#req_message>>)
                        -> Result<tonic::Response<#res_message>, tonic::Status> {
                            Err(tonic::Status::unimplemented("Not yet implemented"))
                        }
                }
            }
            (false, true) => {
                let stream = quote::format_ident!("{}Stream", method.proto_name);
                let stream_doc = generate_doc_comment(&format!(
                    "Server streaming response type for the {} method.",
                    method.proto_name
                ));

                quote! {
                    #stream_doc
                    type #stream: Stream<Item = Result<#res_message, tonic::Status>> + Send + Sync + 'static;

                    #method_doc
                    async fn #name(&self, request: tonic::Request<#req_message>)
                        -> Result<tonic::Response<Self::#stream>, tonic::Status> {
                            Err(tonic::Status::unimplemented("Not yet implemented"))
                        }
                }
            }
            (true, true) => {
                let stream = quote::format_ident!("{}Stream", method.proto_name);
                let stream_doc = generate_doc_comment(&format!(
                    "Server streaming response type for the {} method.",
                    method.proto_name
                ));

                quote! {
                    #stream_doc
                    type #stream: Stream<Item = Result<#res_message, tonic::Status>> + Send + Sync + 'static;

                    #method_doc
                    async fn #name(&self, request: tonic::Request<tonic::Streaming<#req_message>>)
                        -> Result<tonic::Response<Self::#stream>, tonic::Status> {
                            Err(tonic::Status::unimplemented("Not yet implemented"))
                        }
                }
            }
        };

        stream.extend(method);
    }

    stream
}

#[cfg(feature = "transport")]
fn generate_transport(
    server_service: &syn::Ident,
    server_trait: &syn::Ident,
    service_name: &str,
) -> TokenStream {
    let service_name = syn::LitStr::new(service_name, proc_macro2::Span::call_site());

    quote! {
        impl<T: #server_trait> tonic::transport::ServiceName for #server_service<T> {
            const NAME: &'static str = #service_name;
        }
    }
}

#[cfg(not(feature = "transport"))]
fn generate_transport(
    _server_service: &syn::Ident,
    _server_trait: &syn::Ident,
    _service_name: &str,
) -> TokenStream {
    TokenStream::new()
}

fn generate_methods(service: &Service, proto_path: &str) -> TokenStream {
    let mut stream = TokenStream::new();

    for method in &service.methods {
        let path = format!(
            "/{}.{}/{}",
            service.package, service.proto_name, method.proto_name
        );
        let method_path = Lit::Str(LitStr::new(&path, Span::call_site()));
        let ident = quote::format_ident!("{}", method.name);
        let server_trait = quote::format_ident!("{}", service.name);

        let method_stream = match (method.client_streaming, method.server_streaming) {
            (false, false) => generate_unary(method, ident, proto_path, server_trait),

            (false, true) => {
                generate_server_streaming(method, ident.clone(), proto_path, server_trait)
            }
            (true, false) => {
                generate_client_streaming(method, ident.clone(), proto_path, server_trait)
            }

            (true, true) => generate_streaming(method, ident.clone(), proto_path, server_trait),
        };

        let method = quote! {
            #method_path => {
                #method_stream
            }
        };
        stream.extend(method);
    }

    stream
}

fn generate_unary(
    method: &Method,
    method_ident: Ident,
    proto_path: &str,
    server_trait: Ident,
) -> TokenStream {
    let service_ident = quote::format_ident!("{}Svc", method.proto_name);

    let (request, response) = crate::replace_wellknown(proto_path, &method);

    quote! {
        struct #service_ident<T: #server_trait >(pub Arc<T>);

        impl<T: #server_trait> tonic::server::UnaryService<#request> for #service_ident<T> {
            type Response = #response;
            type Future = BoxFuture<tonic::Response<Self::Response>, tonic::Status>;

            fn call(&mut self, request: tonic::Request<#request>) -> Self::Future {
                let inner = self.0.clone();
                let fut = async move {
                    inner.#method_ident(request).await
                };
                Box::pin(fut)
            }
        }

        let inner = self.inner.clone();
        let fut = async move {
            let method = #service_ident(inner);
            let codec = tonic::codec::ProstCodec::default();
            let mut grpc = tonic::server::Grpc::new(codec);
            let res = grpc.unary(method, req).await;
            Ok(res)
        };

        Box::pin(fut)
    }
}

fn generate_server_streaming(
    method: &Method,
    method_ident: Ident,
    proto_path: &str,
    server_trait: Ident,
) -> TokenStream {
    let service_ident = quote::format_ident!("{}Svc", method.proto_name);

    let (request, response) = crate::replace_wellknown(proto_path, &method);

    let response_stream = quote::format_ident!("{}Stream", method.proto_name);

    quote! {
        struct #service_ident<T: #server_trait >(pub Arc<T>);

        impl<T: #server_trait> tonic::server::ServerStreamingService<#request> for #service_ident<T> {
            type Response = #response;
            type ResponseStream = T::#response_stream;
            type Future = BoxFuture<tonic::Response<Self::ResponseStream>, tonic::Status>;

            fn call(&mut self, request: tonic::Request<#request>) -> Self::Future {
                let inner = self.0.clone();
                let fut = async move {
                    inner.#method_ident(request).await

                };
                Box::pin(fut)
            }
        }

        let inner = self.inner.clone();
        let fut = async move {
            let method = #service_ident(inner);
            let codec = tonic::codec::ProstCodec::default();
            let mut grpc = tonic::server::Grpc::new(codec);
            let res = grpc.server_streaming(method, req).await;
            Ok(res)
        };

        Box::pin(fut)
    }
}

fn generate_client_streaming(
    method: &Method,
    method_ident: Ident,
    proto_path: &str,
    server_trait: Ident,
) -> TokenStream {
    let service_ident = quote::format_ident!("{}Svc", method.proto_name);

    let (request, response) = crate::replace_wellknown(proto_path, &method);

    quote! {
        struct #service_ident<T: #server_trait >(pub Arc<T>);

        impl<T: #server_trait> tonic::server::ClientStreamingService<#request> for #service_ident<T>
        {
            type Response = #response;
            type Future = BoxFuture<tonic::Response<Self::Response>, tonic::Status>;

            fn call(&mut self, request: tonic::Request<tonic::Streaming<#request>>) -> Self::Future {
                let inner = self.0.clone();
                let fut = async move {
                    inner.#method_ident(request).await

                };
                Box::pin(fut)
            }
        }

        let inner = self.inner.clone();
        let fut = async move {
            let method = #service_ident(inner);
            let codec = tonic::codec::ProstCodec::default();
            let mut grpc = tonic::server::Grpc::new(codec);
            let res = grpc.client_streaming(method, req).await;
            Ok(res)
        };

        Box::pin(fut)
    }
}

fn generate_streaming(
    method: &Method,
    method_ident: Ident,
    proto_path: &str,
    server_trait: Ident,
) -> TokenStream {
    let service_ident = quote::format_ident!("{}Svc", method.proto_name);

    let (request, response) = crate::replace_wellknown(proto_path, &method);

    let response_stream = quote::format_ident!("{}Stream", method.proto_name);

    quote! {
        struct #service_ident<T: #server_trait>(pub Arc<T>);

        impl<T: #server_trait> tonic::server::StreamingService<#request> for #service_ident<T>
        {
            type Response = #response;
            type ResponseStream = T::#response_stream;
            type Future = BoxFuture<tonic::Response<Self::ResponseStream>, tonic::Status>;

            fn call(&mut self, request: tonic::Request<tonic::Streaming<#request>>) -> Self::Future {
                let inner = self.0.clone();
                let fut = async move {
                    inner.#method_ident(request).await
                };
                Box::pin(fut)
            }
        }

        let inner = self.inner.clone();
        let fut = async move {
            let method = #service_ident(inner);
            let codec = tonic::codec::ProstCodec::default();
            let mut grpc = tonic::server::Grpc::new(codec);
            let res = grpc.streaming(method, req).await;
            Ok(res)
        };

        Box::pin(fut)
    }
}
