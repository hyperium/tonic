use proc_macro2::{Span, TokenStream};
use prost_build::{Method, Service};
use quote::quote;
use syn::{Ident, Lit, LitStr, Path};

pub(crate) fn generate(service: &Service, proto_path: &str) -> TokenStream {
    let methods = generate_methods(&service, proto_path);

    let server_make_service = quote::format_ident!("{}Server", service.name);
    let server_service = quote::format_ident!("{}ServerSvc", service.name);
    let server_trait = quote::format_ident!("{}", service.name);
    let generated_trait = generate_trait(service, proto_path, server_trait.clone());

    quote! {
        #generated_trait

        #[derive(Clone, Debug)]
        pub struct #server_make_service<T: #server_trait> {
            inner: Arc<T>,
        }

        #[derive(Clone, Debug)]
        pub struct #server_service<T: #server_trait> {
            inner: Arc<T>,
        }

        impl<T: #server_trait> #server_make_service<T> {
            pub fn new(inner: T) -> Self {
                let inner = Arc::new(inner);
                Self { inner }
            }
        }

        impl<T: #server_trait> #server_service<T> {
            pub fn new(inner: Arc<T>) -> Self {
                Self { inner }
            }
        }

        impl<T: #server_trait, R> Service<R> for #server_make_service<T> {
            type Response = #server_service<T>;
            type Error = Never;
            type Future = Ready<Result<Self::Response, Self::Error>>;

            fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }

            fn call(&mut self, _: R) -> Self::Future {
                ok(#server_service::new(self.inner.clone()))
            }
        }

        impl<T: #server_trait> Service<http::Request<HyperBody>> for #server_service<T> {
            type Response = http::Response<tonic::BoxBody>;
            type Error = Never;
            type Future = BoxFuture<Self::Response, Self::Error>;

            fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }

            fn call(&mut self, req: http::Request<HyperBody>) -> Self::Future {
                let inner = self.inner.clone();

                match req.uri().path() {
                    #methods

                    // TODO: implement grpc unimplemented for server
                    _ => unimplemented!("use grpc unimplemented"),
                }
            }
        }
    }
}

fn generate_trait(service: &Service, proto_path: &str, server_trait: Ident) -> TokenStream {
    let methods = generate_trait_methods(service, proto_path);

    quote! {
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
        let req_message: Path =
            syn::parse_str(&format!("{}::{}", proto_path, method.input_type)).unwrap();
        let res_message: Path =
            syn::parse_str(&format!("{}::{}", proto_path, method.output_type)).unwrap();

        let method = match (method.client_streaming, method.server_streaming) {
            (false, false) => {
                quote! {
                    async fn #name(&self, request: tonic::Request<#req_message>)
                        -> Result<tonic::Response<#res_message>, tonic::Status>;
                }
            }
            (true, false) => {
                quote! {
                    async fn #name(&self, request: tonic::Request<tonic::Streaming<#req_message>>)
                        -> Result<tonic::Response<#res_message>, tonic::Status>;
                }
            }
            (false, true) => {
                let stream = quote::format_ident!("{}Stream", method.proto_name);

                quote! {
                    type #stream: Stream<Item = Result<#res_message, tonic::Status>>  + Send + 'static;

                    async fn #name(&self, request: tonic::Request<#req_message>)
                        -> Result<tonic::Response<Self::#stream>, tonic::Status>;
                }
            }
            (true, true) => {
                let stream = quote::format_ident!("{}Stream", method.proto_name);

                quote! {
                    type #stream: Stream<Item = Result<#res_message, tonic::Status>> + Send + 'static;

                    async fn #name(&self, request: tonic::Request<tonic::Streaming<#req_message>>)
                        -> Result<tonic::Response<Self::#stream>, tonic::Status>;
                }
            }
        };

        stream.extend(method);
    }

    stream
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
    let service_ident = Ident::new(&method.proto_name, Span::call_site());

    let request: Path = syn::parse_str(&format!("{}::{}", proto_path, method.input_type)).unwrap();
    let response: Path =
        syn::parse_str(&format!("{}::{}", proto_path, method.output_type)).unwrap();

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
            let codec = tonic::codec::ProstCodec::new();
            let mut grpc = tonic::server::Grpc::new(codec);
            let res = grpc.unary(method, req).await;
            Ok(res)
        };

        // TODO: implement this future manually
        Box::pin(fut)
    }
}

fn generate_server_streaming(
    method: &Method,
    method_ident: Ident,
    proto_path: &str,
    server_trait: Ident,
) -> TokenStream {
    let service_ident = Ident::new(&method.proto_name, Span::call_site());

    let request: Path = syn::parse_str(&format!("{}::{}", proto_path, method.input_type)).unwrap();
    let response: Path =
        syn::parse_str(&format!("{}::{}", proto_path, method.output_type)).unwrap();

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
            let codec = tonic::codec::ProstCodec::new();
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
    let service_ident = Ident::new(&method.proto_name, Span::call_site());

    let request: Path = syn::parse_str(&format!("{}::{}", proto_path, method.input_type)).unwrap();
    let response: Path =
        syn::parse_str(&format!("{}::{}", proto_path, method.output_type)).unwrap();

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
            let codec = tonic::codec::ProstCodec::new();
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
    let service_ident = Ident::new(&method.proto_name, Span::call_site());

    let request: Path = syn::parse_str(&format!("{}::{}", proto_path, method.input_type)).unwrap();
    let response: Path =
        syn::parse_str(&format!("{}::{}", proto_path, method.output_type)).unwrap();

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
            let codec = tonic::codec::ProstCodec::new();
            let mut grpc = tonic::server::Grpc::new(codec);
            let res = grpc.streaming(method, req).await;
            Ok(res)
        };

        Box::pin(fut)
    }
}
