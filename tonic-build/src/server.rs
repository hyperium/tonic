use super::{Method, Service};
use crate::{generate_doc_comment, generate_doc_comments, naive_snake_case};
use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{Ident, Lit, LitStr};

/// Generate service for Server.
///
/// This takes some `Service` and will generate a `TokenStream` that contains
/// a public module containing the server service and handler trait.
pub fn generate<T: Service>(
    service: &T,
    emit_package: bool,
    proto_path: &str,
    compile_well_known_types: bool,
) -> TokenStream {
    let methods = generate_methods(service, proto_path, compile_well_known_types);

    let server_service = quote::format_ident!("{}Server", service.name());
    let server_trait = quote::format_ident!("{}", service.name());
    let server_mod = quote::format_ident!("{}_server", naive_snake_case(&service.name()));
    let generated_trait = generate_trait(
        service,
        proto_path,
        compile_well_known_types,
        server_trait.clone(),
    );
    let service_doc = generate_doc_comments(service.comment());
    let package = if emit_package { service.package() } else { "" };
    // Transport based implementations
    let path = format!(
        "{}{}{}",
        package,
        if package.is_empty() { "" } else { "." },
        service.identifier()
    );
    let transport = generate_transport(&server_service, &server_trait, &path);

    quote! {
        /// Generated server implementations.
        pub mod #server_mod {
            #![allow(unused_variables, dead_code, missing_docs)]
            use tonic::codegen::*;

            #generated_trait

            #service_doc
            #[derive(Debug)]
            pub struct #server_service<T: #server_trait> {
                inner: _Inner<T>,
            }

            struct _Inner<T>(Arc<T>, Option<tonic::Interceptor>);

            impl<T: #server_trait> #server_service<T> {
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

            impl<T, B> Service<http::Request<B>> for #server_service<T>
                where
                    T: #server_trait,
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
                        #methods

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

            impl<T: #server_trait> Clone for #server_service<T> {
                fn clone(&self) -> Self {
                    let inner = self.inner.clone();
                    Self { inner }
                }
            }

            impl<T: #server_trait> Clone for _Inner<T> {
                fn clone(&self) -> Self {
                    Self(self.0.clone(), self.1.clone())
                }
            }

            impl<T: std::fmt::Debug> std::fmt::Debug for _Inner<T> {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                   write!(f, "{:?}", self.0)
                }
            }

            #transport
        }
    }
}

fn generate_trait<T: Service>(
    service: &T,
    proto_path: &str,
    compile_well_known_types: bool,
    server_trait: Ident,
) -> TokenStream {
    let methods = generate_trait_methods(service, proto_path, compile_well_known_types);
    let trait_doc = generate_doc_comment(&format!(
        "Generated trait containing gRPC methods that should be implemented for use with {}Server.",
        service.name()
    ));

    quote! {
        #trait_doc
        #[async_trait]
        pub trait #server_trait : Send + Sync + 'static {
            #methods
        }
    }
}

fn generate_trait_methods<T: Service>(
    service: &T,
    proto_path: &str,
    compile_well_known_types: bool,
) -> TokenStream {
    let mut stream = TokenStream::new();

    for method in service.methods() {
        let name = quote::format_ident!("{}", method.name());

        let (req_message, res_message) =
            method.request_response_name(proto_path, compile_well_known_types);

        let method_doc = generate_doc_comments(method.comment());

        let method = match (method.client_streaming(), method.server_streaming()) {
            (false, false) => {
                quote! {
                    #method_doc
                    async fn #name(&self, request: tonic::Request<#req_message>)
                        -> Result<tonic::Response<#res_message>, tonic::Status>;
                }
            }
            (true, false) => {
                quote! {
                    #method_doc
                    async fn #name(&self, request: tonic::Request<tonic::Streaming<#req_message>>)
                        -> Result<tonic::Response<#res_message>, tonic::Status>;
                }
            }
            (false, true) => {
                let stream = quote::format_ident!("{}Stream", method.identifier());
                let stream_doc = generate_doc_comment(&format!(
                    "Server streaming response type for the {} method.",
                    method.identifier()
                ));

                quote! {
                    #stream_doc
                    type #stream: Stream<Item = Result<#res_message, tonic::Status>> + Send + Sync + 'static;

                    #method_doc
                    async fn #name(&self, request: tonic::Request<#req_message>)
                        -> Result<tonic::Response<Self::#stream>, tonic::Status>;
                }
            }
            (true, true) => {
                let stream = quote::format_ident!("{}Stream", method.identifier());
                let stream_doc = generate_doc_comment(&format!(
                    "Server streaming response type for the {} method.",
                    method.identifier()
                ));

                quote! {
                    #stream_doc
                    type #stream: Stream<Item = Result<#res_message, tonic::Status>> + Send + Sync + 'static;

                    #method_doc
                    async fn #name(&self, request: tonic::Request<tonic::Streaming<#req_message>>)
                        -> Result<tonic::Response<Self::#stream>, tonic::Status>;
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
        impl<T: #server_trait> tonic::transport::NamedService for #server_service<T> {
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

fn generate_methods<T: Service>(
    service: &T,
    proto_path: &str,
    compile_well_known_types: bool,
) -> TokenStream {
    let mut stream = TokenStream::new();

    for method in service.methods() {
        let path = format!(
            "/{}{}{}/{}",
            service.package(),
            if service.package().is_empty() {
                ""
            } else {
                "."
            },
            service.identifier(),
            method.identifier()
        );
        let method_path = Lit::Str(LitStr::new(&path, Span::call_site()));
        let ident = quote::format_ident!("{}", method.name());
        let server_trait = quote::format_ident!("{}", service.name());

        let method_stream = match (method.client_streaming(), method.server_streaming()) {
            (false, false) => generate_unary(
                method,
                proto_path,
                compile_well_known_types,
                ident,
                server_trait,
            ),

            (false, true) => generate_server_streaming(
                method,
                proto_path,
                compile_well_known_types,
                ident.clone(),
                server_trait,
            ),
            (true, false) => generate_client_streaming(
                method,
                proto_path,
                compile_well_known_types,
                ident.clone(),
                server_trait,
            ),

            (true, true) => generate_streaming(
                method,
                proto_path,
                compile_well_known_types,
                ident.clone(),
                server_trait,
            ),
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

fn generate_unary<T: Method>(
    method: &T,
    proto_path: &str,
    compile_well_known_types: bool,
    method_ident: Ident,
    server_trait: Ident,
) -> TokenStream {
    let codec_name = syn::parse_str::<syn::Path>(T::CODEC_PATH).unwrap();

    let service_ident = quote::format_ident!("{}Svc", method.identifier());

    let (request, response) = method.request_response_name(proto_path, compile_well_known_types);

    quote! {
        #[allow(non_camel_case_types)]
        struct #service_ident<T: #server_trait >(pub Arc<T>);

        impl<T: #server_trait> tonic::server::UnaryService<#request> for #service_ident<T> {
            type Response = #response;
            type Future = BoxFuture<tonic::Response<Self::Response>, tonic::Status>;

            fn call(&mut self, request: tonic::Request<#request>) -> Self::Future {
                let inner = self.0.clone();
                let fut = async move {
                    (*inner).#method_ident(request).await
                };
                Box::pin(fut)
            }
        }

        let inner = self.inner.clone();
        let fut = async move {
            let interceptor = inner.1.clone();
            let inner = inner.0;
            let method = #service_ident(inner);
            let codec = #codec_name::default();

            let mut grpc = if let Some(interceptor) = interceptor {
                tonic::server::Grpc::with_interceptor(codec, interceptor)
            } else {
                tonic::server::Grpc::new(codec)
            };

            let res = grpc.unary(method, req).await;
            Ok(res)
        };

        Box::pin(fut)
    }
}

fn generate_server_streaming<T: Method>(
    method: &T,
    proto_path: &str,
    compile_well_known_types: bool,
    method_ident: Ident,
    server_trait: Ident,
) -> TokenStream {
    let codec_name = syn::parse_str::<syn::Path>(T::CODEC_PATH).unwrap();

    let service_ident = quote::format_ident!("{}Svc", method.identifier());

    let (request, response) = method.request_response_name(proto_path, compile_well_known_types);

    let response_stream = quote::format_ident!("{}Stream", method.identifier());

    quote! {
        #[allow(non_camel_case_types)]
        struct #service_ident<T: #server_trait >(pub Arc<T>);

        impl<T: #server_trait> tonic::server::ServerStreamingService<#request> for #service_ident<T> {
            type Response = #response;
            type ResponseStream = T::#response_stream;
            type Future = BoxFuture<tonic::Response<Self::ResponseStream>, tonic::Status>;

            fn call(&mut self, request: tonic::Request<#request>) -> Self::Future {
                let inner = self.0.clone();
                let fut = async move {
                    (*inner).#method_ident(request).await

                };
                Box::pin(fut)
            }
        }

        let inner = self.inner.clone();
        let fut = async move {
            let interceptor = inner.1;
            let inner = inner.0;
            let method = #service_ident(inner);
            let codec = #codec_name::default();

            let mut grpc = if let Some(interceptor) = interceptor {
                tonic::server::Grpc::with_interceptor(codec, interceptor)
            } else {
                tonic::server::Grpc::new(codec)
            };

            let res = grpc.server_streaming(method, req).await;
            Ok(res)
        };

        Box::pin(fut)
    }
}

fn generate_client_streaming<T: Method>(
    method: &T,
    proto_path: &str,
    compile_well_known_types: bool,
    method_ident: Ident,
    server_trait: Ident,
) -> TokenStream {
    let service_ident = quote::format_ident!("{}Svc", method.identifier());

    let (request, response) = method.request_response_name(proto_path, compile_well_known_types);
    let codec_name = syn::parse_str::<syn::Path>(T::CODEC_PATH).unwrap();

    quote! {
        #[allow(non_camel_case_types)]
        struct #service_ident<T: #server_trait >(pub Arc<T>);

        impl<T: #server_trait> tonic::server::ClientStreamingService<#request> for #service_ident<T>
        {
            type Response = #response;
            type Future = BoxFuture<tonic::Response<Self::Response>, tonic::Status>;

            fn call(&mut self, request: tonic::Request<tonic::Streaming<#request>>) -> Self::Future {
                let inner = self.0.clone();
                let fut = async move {
                    (*inner).#method_ident(request).await

                };
                Box::pin(fut)
            }
        }

        let inner = self.inner.clone();
        let fut = async move {
            let interceptor = inner.1;
            let inner = inner.0;
            let method = #service_ident(inner);
            let codec = #codec_name::default();

            let mut grpc = if let Some(interceptor) = interceptor {
                tonic::server::Grpc::with_interceptor(codec, interceptor)
            } else {
                tonic::server::Grpc::new(codec)
            };

            let res = grpc.client_streaming(method, req).await;
            Ok(res)
        };

        Box::pin(fut)
    }
}

fn generate_streaming<T: Method>(
    method: &T,
    proto_path: &str,
    compile_well_known_types: bool,
    method_ident: Ident,
    server_trait: Ident,
) -> TokenStream {
    let codec_name = syn::parse_str::<syn::Path>(T::CODEC_PATH).unwrap();

    let service_ident = quote::format_ident!("{}Svc", method.identifier());

    let (request, response) = method.request_response_name(proto_path, compile_well_known_types);

    let response_stream = quote::format_ident!("{}Stream", method.identifier());

    quote! {
        #[allow(non_camel_case_types)]
        struct #service_ident<T: #server_trait>(pub Arc<T>);

        impl<T: #server_trait> tonic::server::StreamingService<#request> for #service_ident<T>
        {
            type Response = #response;
            type ResponseStream = T::#response_stream;
            type Future = BoxFuture<tonic::Response<Self::ResponseStream>, tonic::Status>;

            fn call(&mut self, request: tonic::Request<tonic::Streaming<#request>>) -> Self::Future {
                let inner = self.0.clone();
                let fut = async move {
                    (*inner).#method_ident(request).await
                };
                Box::pin(fut)
            }
        }

        let inner = self.inner.clone();
        let fut = async move {
            let interceptor = inner.1;
            let inner = inner.0;
            let method = #service_ident(inner);
            let codec = #codec_name::default();

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
}
