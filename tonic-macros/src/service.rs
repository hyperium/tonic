use crate::{Method, Service};
use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{Ident, Lit, LitStr, Path};

pub(crate) fn generate(service: Service, proto_path: &str) -> TokenStream {
    let methods = generate_methods(&service, proto_path);

    let server_make_service = quote::format_ident!("{}Server", service.name);
    let server_service = quote::format_ident!("{}ServerSvc", service.name);
    let server_trait = quote::format_ident!("{}", service.name);

    quote! {
        use tonic::_codegen::*;

        #[async_trait]
        pub trait #server_trait : Clone + Send + 'static {
            async fn say_hello(self, req: tonic::Request<self::HelloRequest>)
                -> Result<tonic::Response<self::HelloReply>, tonic::Status>;
        }

        // TODO: impl debug
        #[derive(Clone)]
        pub struct #server_make_service <T: #server_trait > {
            inner: T,
        }

         // TODO: impl debug
        pub struct #server_service <T: #server_trait > {
            inner: T,
        }

        impl<T: #server_trait > #server_make_service <T> {
            pub fn new(inner: T) -> Self {
                Self { inner }
            }
        }

        impl<T: #server_trait > #server_service <T> {
            pub fn new(inner: T) -> Self {
                Self { inner }
            }
        }

        impl<T: #server_trait , R> Service<R> for #server_make_service <T> {
            type Response = #server_service <T>;
            type Error = tonic::error::Never;
            type Future = Ready<Result<Self::Response, Self::Error>>;

            fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }

            fn call(&mut self, _: R) -> Self::Future {
                ok(#server_service ::new(self.inner.clone()))
            }
        }

        impl<T: #server_trait > Service<http::Request<tonic::_codegen::HyperBody>> for #server_service <T> {
            type Response = http::Response<tonic::BoxBody>;
            type Error = tonic::error::Never;
            type Future = BoxFuture<Self::Response, Self::Error>;

            fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }

            fn call(&mut self, req: http::Request<tonic::_codegen::HyperBody>) -> Self::Future {
                let inner = self.inner.clone();

                match req.uri().path() {
                    #methods

                    _ => unimplemented!("use grpc unimplemented"),
                }
            }
        }
    }
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
            (false, false) => generate_unary(
                method,
                ident,
                proto_path,
                server_trait
            ),

            _ => unimplemented!()

            // (false, true) => generate_server_streaming(
            //     method,
            //     ident.clone(),
            //     service.name.clone(),
            //     &service.proto_path,
            // ),

            // (true, false) => generate_client_streaming(
            //     method,
            //     ident.clone(),
            //     service.name.clone(),
            //     &service.proto_path,
            // ),

            // (true, true) => generate_streaming(
            //     method,
            //     ident.clone(),
            //     service.name.clone(),
            //     &service.proto_path,
            // ),
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
        struct #service_ident <T: #server_trait >(pub T);

        impl<T: #server_trait > tonic::server::UnaryService<#request> for #service_ident <T> {
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

// fn generate_server_streaming(
//     method: &Method,
//     method_ident: Ident,
//     service_impl: Path,
//     proto_path: &str,
// ) -> TokenStream {
//     let service_ident = Ident::new(&method.proto_name, Span::call_site());

//     let request: Path = syn::parse_str(&format!("{}::{}", proto_path, method.input_type)).unwrap();
//     let response: Path =
//         syn::parse_str(&format!("{}::{}", proto_path, method.output_type)).unwrap();

//     // TODO: parse response stream type, if it is a concrete type then use that
//     // as the ResponseStream type, if it is a impl Trait then we need to box.
//     quote! {
//         struct #service_ident(pub std::sync::Arc<#service_impl>);

//         impl tonic::server::ServerStreamingService<#request> for #service_ident {
//             type Response = #response;
//             type ResponseStream = Pin<Box<dyn Stream<Item = Result<Self::Response, Status>> + Send>>;
//             type Future = BoxFuture<tonic::Response<Self::ResponseStream>, tonic::Status>;

//             fn call(&mut self, request: tonic::Request<#request>) -> Self::Future {
//                 let inner = self.0.clone();
//                 let fut = async move {
//                     inner.#method_ident(request)
//                         .await
//                         .map(|r|
//                             r.map(|s| Box::pin(s) as Pin<Box<dyn Stream<Item = Result<Self::Response, Status>> + Send>>))

//                 };
//                 Box::pin(fut)
//             }
//         }

//         let inner = self.inner.clone();
//         let fut = async move {
//             let method = #service_ident(inner);
//             let codec = tonic::codec::ProstCodec::new();
//             let mut grpc = tonic::server::Grpc::new(codec);
//             let res = grpc.server_streaming(method, req).await;
//             Ok(res)
//         };

//         Box::pin(fut)
//     }
// }

// fn generate_client_streaming(
//     method: &Method,
//     method_ident: Ident,
//     service_impl: Path,
//     proto_path: &str,
// ) -> TokenStream {
//     let service_ident = Ident::new(&method.proto_name, Span::call_site());

//     let request: Path = syn::parse_str(&format!("{}::{}", proto_path, method.input_type)).unwrap();
//     let response: Path =
//         syn::parse_str(&format!("{}::{}", proto_path, method.output_type)).unwrap();

//     quote! {
//         struct #service_ident(pub std::sync::Arc<#service_impl>);

//         impl<S> tonic::server::ClientStreamingService<S> for #service_ident
//         where S: tonic::_codegen::Stream<Item = Result<#request, Status>> + Unpin + Send + 'static {
//             type Response = #response;
//             type Future = BoxFuture<tonic::Response<Self::Response>, tonic::Status>;

//             fn call(&mut self, request: tonic::Request<S>) -> Self::Future {
//                 let inner = self.0.clone();
//                 let fut = async move {
//                     inner.#method_ident(request).await

//                 };
//                 Box::pin(fut)
//             }
//         }

//         let inner = self.inner.clone();
//         let fut = async move {
//             let method = #service_ident(inner);
//             let codec = tonic::codec::ProstCodec::new();
//             let mut grpc = tonic::server::Grpc::new(codec);
//             let res = grpc.client_streaming(method, req).await;
//             Ok(res)
//         };

//         Box::pin(fut)
//     }
// }

// fn generate_streaming(
//     method: &Method,
//     method_ident: Ident,
//     service_impl: Path,
//     proto_path: &str,
// ) -> TokenStream {
//     let service_ident = Ident::new(&method.proto_name, Span::call_site());

//     let request: Path = syn::parse_str(&format!("{}::{}", proto_path, method.input_type)).unwrap();
//     let response: Path =
//         syn::parse_str(&format!("{}::{}", proto_path, method.output_type)).unwrap();

//     // TODO: parse response stream type, if it is a concrete type then use that
//     // as the ResponseStream type, if it is a impl Trait then we need to box.
//     quote! {
//         struct #service_ident(pub std::sync::Arc<#service_impl>);

//         impl<S> tonic::server::StreamingService<S> for #service_ident
//         where S: Stream<Item = Result<#request, Status>> + Unpin + Send + 'static {
//             type Response = #response;
//             type ResponseStream = Pin<Box<dyn Stream<Item = Result<Self::Response, Status>> + Send>>;
//             type Future = BoxFuture<tonic::Response<Self::ResponseStream>, tonic::Status>;

//             fn call(&mut self, request: tonic::Request<S>) -> Self::Future {
//                 let inner = self.0.clone();
//                 let fut = async move {
//                     inner.#method_ident(request).await
//                         .map(|r|
//                             r.map(|s| Box::pin(s) as Pin<Box<dyn Stream<Item = Result<Self::Response, Status>> + Send>>))

//                 };
//                 Box::pin(fut)
//             }
//         }

//         let inner = self.inner.clone();
//         let fut = async move {
//             let method = #service_ident(inner);
//             let codec = tonic::codec::ProstCodec::new();
//             let mut grpc = tonic::server::Grpc::new(codec);
//             let res = grpc.streaming(method, req).await;
//             Ok(res)
//         };

//         Box::pin(fut)
//     }
// }
