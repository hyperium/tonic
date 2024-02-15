use std::collections::HashSet;

use super::{Attributes, Method, Service};
use crate::{
    format_method_name, format_method_path, format_service_name, generate_doc_comment,
    generate_doc_comments, naive_snake_case,
};
use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{Ident, Lit, LitStr};

#[allow(clippy::too_many_arguments)]
pub(crate) fn generate_internal<T: Service>(
    service: &T,
    emit_package: bool,
    proto_path: &str,
    compile_well_known_types: bool,
    attributes: &Attributes,
    disable_comments: &HashSet<String>,
    use_arc_self: bool,
    generate_default_stubs: bool,
) -> TokenStream {
    let methods = generate_methods(
        service,
        emit_package,
        proto_path,
        compile_well_known_types,
        use_arc_self,
        generate_default_stubs,
    );

    let server_service = quote::format_ident!("{}Server", service.name());
    let server_trait = quote::format_ident!("{}", service.name());
    let server_mod = quote::format_ident!("{}_server", naive_snake_case(service.name()));
    let generated_trait = generate_trait(
        service,
        emit_package,
        proto_path,
        compile_well_known_types,
        server_trait.clone(),
        disable_comments,
        use_arc_self,
        generate_default_stubs,
    );
    let package = if emit_package { service.package() } else { "" };
    // Transport based implementations
    let service_name = format_service_name(service, emit_package);

    let service_doc = if disable_comments.contains(&service_name) {
        TokenStream::new()
    } else {
        generate_doc_comments(service.comment())
    };

    let named = generate_named(&server_service, &server_trait, &service_name);
    let mod_attributes = attributes.for_mod(package);
    let struct_attributes = attributes.for_struct(&service_name);

    let configure_compression_methods = quote! {
        /// Enable decompressing requests with the given encoding.
        #[must_use]
        pub fn accept_compressed(mut self, encoding: CompressionEncoding) -> Self {
            self.accept_compression_encodings.enable(encoding);
            self
        }

        /// Compress responses with the given encoding, if the client supports it.
        #[must_use]
        pub fn send_compressed(mut self, encoding: CompressionEncoding) -> Self {
            self.send_compression_encodings.enable(encoding);
            self
        }
    };

    let configure_max_message_size_methods = quote! {
        /// Limits the maximum size of a decoded message.
        ///
        /// Default: `4MB`
        #[must_use]
        pub fn max_decoding_message_size(mut self, limit: usize) -> Self {
            self.max_decoding_message_size = Some(limit);
            self
        }

        /// Limits the maximum size of an encoded message.
        ///
        /// Default: `usize::MAX`
        #[must_use]
        pub fn max_encoding_message_size(mut self, limit: usize) -> Self {
            self.max_encoding_message_size = Some(limit);
            self
        }
    };

    quote! {
        /// Generated server implementations.
        #(#mod_attributes)*
        pub mod #server_mod {
            #![allow(
                unused_variables,
                dead_code,
                missing_docs,
                // will trigger if compression is disabled
                clippy::let_unit_value,
            )]
            use tonic::codegen::*;

            #generated_trait

            #service_doc
            #(#struct_attributes)*
            #[derive(Debug)]
            pub struct #server_service<T: #server_trait> {
                inner: _Inner<T>,
                accept_compression_encodings: EnabledCompressionEncodings,
                send_compression_encodings: EnabledCompressionEncodings,
                max_decoding_message_size: Option<usize>,
                max_encoding_message_size: Option<usize>,
            }

            struct _Inner<T>(Arc<T>);

            impl<T: #server_trait> #server_service<T> {
                pub fn new(inner: T) -> Self {
                    Self::from_arc(Arc::new(inner))
                }

                pub fn from_arc(inner: Arc<T>) -> Self {
                    let inner = _Inner(inner);
                    Self {
                        inner,
                        accept_compression_encodings: Default::default(),
                        send_compression_encodings: Default::default(),
                        max_decoding_message_size: None,
                        max_encoding_message_size: None,
                    }
                }

                pub fn with_interceptor<F>(inner: T, interceptor: F) -> InterceptedService<Self, F>
                where
                    F: tonic::service::Interceptor,
                {
                    InterceptedService::new(Self::new(inner), interceptor)
                }

                #configure_compression_methods

                #configure_max_message_size_methods
            }

            impl<T, B> tonic::codegen::Service<http::Request<B>> for #server_service<T>
                where
                    T: #server_trait,
                    B: Body + Send + 'static,
                    B::Error: Into<StdError> + Send + 'static,
            {
                type Response = http::Response<tonic::body::BoxBody>;
                type Error = std::convert::Infallible;
                type Future = BoxFuture<Self::Response, Self::Error>;

                fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<std::result::Result<(), Self::Error>> {
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
                               .body(empty_body())
                               .unwrap())
                        }),
                    }
                }
            }

            impl<T: #server_trait> Clone for #server_service<T> {
                fn clone(&self) -> Self {
                    let inner = self.inner.clone();
                    Self {
                        inner,
                        accept_compression_encodings: self.accept_compression_encodings,
                        send_compression_encodings: self.send_compression_encodings,
                        max_decoding_message_size: self.max_decoding_message_size,
                        max_encoding_message_size: self.max_encoding_message_size,
                    }
                }
            }

            impl<T: #server_trait> Clone for _Inner<T> {
                fn clone(&self) -> Self {
                    Self(Arc::clone(&self.0))
                }
            }

            impl<T: std::fmt::Debug> std::fmt::Debug for _Inner<T> {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                   write!(f, "{:?}", self.0)
                }
            }

            #named
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn generate_trait<T: Service>(
    service: &T,
    emit_package: bool,
    proto_path: &str,
    compile_well_known_types: bool,
    server_trait: Ident,
    disable_comments: &HashSet<String>,
    use_arc_self: bool,
    generate_default_stubs: bool,
) -> TokenStream {
    let methods = generate_trait_methods(
        service,
        emit_package,
        proto_path,
        compile_well_known_types,
        disable_comments,
        use_arc_self,
        generate_default_stubs,
    );
    let trait_doc = generate_doc_comment(format!(
        " Generated trait containing gRPC methods that should be implemented for use with {}Server.",
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
    emit_package: bool,
    proto_path: &str,
    compile_well_known_types: bool,
    disable_comments: &HashSet<String>,
    use_arc_self: bool,
    generate_default_stubs: bool,
) -> TokenStream {
    let mut stream = TokenStream::new();

    for method in service.methods() {
        let name = quote::format_ident!("{}", method.name());

        let (req_message, res_message) =
            method.request_response_name(proto_path, compile_well_known_types);

        let method_doc =
            if disable_comments.contains(&format_method_name(service, method, emit_package)) {
                TokenStream::new()
            } else {
                generate_doc_comments(method.comment())
            };

        let self_param = if use_arc_self {
            quote!(self: std::sync::Arc<Self>)
        } else {
            quote!(&self)
        };

        let method = match (
            method.client_streaming(),
            method.server_streaming(),
            generate_default_stubs,
        ) {
            (false, false, true) => {
                quote! {
                    #method_doc
                    async fn #name(#self_param, request: tonic::Request<#req_message>)
                        -> std::result::Result<tonic::Response<#res_message>, tonic::Status> {
                        Err(tonic::Status::unimplemented("Not yet implemented"))
                    }
                }
            }
            (false, false, false) => {
                quote! {
                    #method_doc
                    async fn #name(#self_param, request: tonic::Request<#req_message>)
                        -> std::result::Result<tonic::Response<#res_message>, tonic::Status>;
                }
            }
            (true, false, true) => {
                quote! {
                    #method_doc
                    async fn #name(#self_param, request: tonic::Request<tonic::Streaming<#req_message>>)
                        -> std::result::Result<tonic::Response<#res_message>, tonic::Status> {
                        Err(tonic::Status::unimplemented("Not yet implemented"))
                    }
                }
            }
            (true, false, false) => {
                quote! {
                    #method_doc
                    async fn #name(#self_param, request: tonic::Request<tonic::Streaming<#req_message>>)
                        -> std::result::Result<tonic::Response<#res_message>, tonic::Status>;
                }
            }
            (false, true, true) => {
                quote! {
                    #method_doc
                    async fn #name(#self_param, request: tonic::Request<#req_message>)
                        -> std::result::Result<tonic::Response<BoxStream<#res_message>>, tonic::Status> {
                        Err(tonic::Status::unimplemented("Not yet implemented"))
                    }
                }
            }
            (false, true, false) => {
                let stream = quote::format_ident!("{}Stream", method.identifier());
                let stream_doc = generate_doc_comment(format!(
                    " Server streaming response type for the {} method.",
                    method.identifier()
                ));

                quote! {
                    #stream_doc
                    type #stream: tonic::codegen::tokio_stream::Stream<Item = std::result::Result<#res_message, tonic::Status>> + Send + 'static;

                    #method_doc
                    async fn #name(#self_param, request: tonic::Request<#req_message>)
                        -> std::result::Result<tonic::Response<Self::#stream>, tonic::Status>;
                }
            }
            (true, true, true) => {
                quote! {
                    #method_doc
                    async fn #name(#self_param, request: tonic::Request<tonic::Streaming<#req_message>>)
                        -> std::result::Result<tonic::Response<BoxStream<#res_message>>, tonic::Status> {
                        Err(tonic::Status::unimplemented("Not yet implemented"))
                    }
                }
            }
            (true, true, false) => {
                let stream = quote::format_ident!("{}Stream", method.identifier());
                let stream_doc = generate_doc_comment(format!(
                    " Server streaming response type for the {} method.",
                    method.identifier()
                ));

                quote! {
                    #stream_doc
                    type #stream: tonic::codegen::tokio_stream::Stream<Item = std::result::Result<#res_message, tonic::Status>> + Send + 'static;

                    #method_doc
                    async fn #name(#self_param, request: tonic::Request<tonic::Streaming<#req_message>>)
                        -> std::result::Result<tonic::Response<Self::#stream>, tonic::Status>;
                }
            }
        };

        stream.extend(method);
    }

    stream
}

fn generate_named(
    server_service: &syn::Ident,
    server_trait: &syn::Ident,
    service_name: &str,
) -> TokenStream {
    let service_name = syn::LitStr::new(service_name, proc_macro2::Span::call_site());

    quote! {
        impl<T: #server_trait> tonic::server::NamedService for #server_service<T> {
            const NAME: &'static str = #service_name;
        }
    }
}

fn generate_methods<T: Service>(
    service: &T,
    emit_package: bool,
    proto_path: &str,
    compile_well_known_types: bool,
    use_arc_self: bool,
    generate_default_stubs: bool,
) -> TokenStream {
    let mut stream = TokenStream::new();

    for method in service.methods() {
        let path = format_method_path(service, method, emit_package);
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
                use_arc_self,
            ),

            (false, true) => generate_server_streaming(
                method,
                proto_path,
                compile_well_known_types,
                ident.clone(),
                server_trait,
                use_arc_self,
                generate_default_stubs,
            ),
            (true, false) => generate_client_streaming(
                method,
                proto_path,
                compile_well_known_types,
                ident.clone(),
                server_trait,
                use_arc_self,
            ),

            (true, true) => generate_streaming(
                method,
                proto_path,
                compile_well_known_types,
                ident.clone(),
                server_trait,
                use_arc_self,
                generate_default_stubs,
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
    use_arc_self: bool,
) -> TokenStream {
    let codec_name = syn::parse_str::<syn::Path>(method.codec_path()).unwrap();

    let service_ident = quote::format_ident!("{}Svc", method.identifier());

    let (request, response) = method.request_response_name(proto_path, compile_well_known_types);

    let inner_arg = if use_arc_self {
        quote!(inner)
    } else {
        quote!(&inner)
    };

    quote! {
        #[allow(non_camel_case_types)]
        struct #service_ident<T: #server_trait >(pub Arc<T>);

        impl<T: #server_trait> tonic::server::UnaryService<#request> for #service_ident<T> {
            type Response = #response;
            type Future = BoxFuture<tonic::Response<Self::Response>, tonic::Status>;

            fn call(&mut self, request: tonic::Request<#request>) -> Self::Future {
                let inner = Arc::clone(&self.0);
                let fut = async move {
                    <T as #server_trait>::#method_ident(#inner_arg, request).await
                };
                Box::pin(fut)
            }
        }

        let accept_compression_encodings = self.accept_compression_encodings;
        let send_compression_encodings = self.send_compression_encodings;
        let max_decoding_message_size = self.max_decoding_message_size;
        let max_encoding_message_size = self.max_encoding_message_size;
        let inner = self.inner.clone();
        let fut = async move {
            let inner = inner.0;
            let method = #service_ident(inner);
            let codec = #codec_name::default();

            let mut grpc = tonic::server::Grpc::new(codec)
                .apply_compression_config(accept_compression_encodings, send_compression_encodings)
                .apply_max_message_size_config(max_decoding_message_size, max_encoding_message_size);

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
    use_arc_self: bool,
    generate_default_stubs: bool,
) -> TokenStream {
    let codec_name = syn::parse_str::<syn::Path>(method.codec_path()).unwrap();

    let service_ident = quote::format_ident!("{}Svc", method.identifier());

    let (request, response) = method.request_response_name(proto_path, compile_well_known_types);

    let response_stream = if !generate_default_stubs {
        let stream = quote::format_ident!("{}Stream", method.identifier());
        quote!(type ResponseStream = T::#stream)
    } else {
        quote!(type ResponseStream = BoxStream<#response>)
    };

    let inner_arg = if use_arc_self {
        quote!(inner)
    } else {
        quote!(&inner)
    };

    quote! {
        #[allow(non_camel_case_types)]
        struct #service_ident<T: #server_trait >(pub Arc<T>);

        impl<T: #server_trait> tonic::server::ServerStreamingService<#request> for #service_ident<T> {
            type Response = #response;
            #response_stream;
            type Future = BoxFuture<tonic::Response<Self::ResponseStream>, tonic::Status>;

            fn call(&mut self, request: tonic::Request<#request>) -> Self::Future {
                let inner = Arc::clone(&self.0);
                let fut = async move {
                    <T as #server_trait>::#method_ident(#inner_arg, request).await
                };
                Box::pin(fut)
            }
        }

        let accept_compression_encodings = self.accept_compression_encodings;
        let send_compression_encodings = self.send_compression_encodings;
        let max_decoding_message_size = self.max_decoding_message_size;
        let max_encoding_message_size = self.max_encoding_message_size;
        let inner = self.inner.clone();
        let fut = async move {
            let inner = inner.0;
            let method = #service_ident(inner);
            let codec = #codec_name::default();

            let mut grpc = tonic::server::Grpc::new(codec)
                .apply_compression_config(accept_compression_encodings, send_compression_encodings)
                .apply_max_message_size_config(max_decoding_message_size, max_encoding_message_size);

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
    use_arc_self: bool,
) -> TokenStream {
    let service_ident = quote::format_ident!("{}Svc", method.identifier());

    let (request, response) = method.request_response_name(proto_path, compile_well_known_types);
    let codec_name = syn::parse_str::<syn::Path>(method.codec_path()).unwrap();

    let inner_arg = if use_arc_self {
        quote!(inner)
    } else {
        quote!(&inner)
    };

    quote! {
        #[allow(non_camel_case_types)]
        struct #service_ident<T: #server_trait >(pub Arc<T>);

        impl<T: #server_trait> tonic::server::ClientStreamingService<#request> for #service_ident<T>
        {
            type Response = #response;
            type Future = BoxFuture<tonic::Response<Self::Response>, tonic::Status>;

            fn call(&mut self, request: tonic::Request<tonic::Streaming<#request>>) -> Self::Future {
                let inner = Arc::clone(&self.0);
                let fut = async move {
                    <T as #server_trait>::#method_ident(#inner_arg, request).await
                };
                Box::pin(fut)
            }
        }

        let accept_compression_encodings = self.accept_compression_encodings;
        let send_compression_encodings = self.send_compression_encodings;
        let max_decoding_message_size = self.max_decoding_message_size;
        let max_encoding_message_size = self.max_encoding_message_size;
        let inner = self.inner.clone();
        let fut = async move {
            let inner = inner.0;
            let method = #service_ident(inner);
            let codec = #codec_name::default();

            let mut grpc = tonic::server::Grpc::new(codec)
                .apply_compression_config(accept_compression_encodings, send_compression_encodings)
                .apply_max_message_size_config(max_decoding_message_size, max_encoding_message_size);

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
    use_arc_self: bool,
    generate_default_stubs: bool,
) -> TokenStream {
    let codec_name = syn::parse_str::<syn::Path>(method.codec_path()).unwrap();

    let service_ident = quote::format_ident!("{}Svc", method.identifier());

    let (request, response) = method.request_response_name(proto_path, compile_well_known_types);

    let response_stream = if !generate_default_stubs {
        let stream = quote::format_ident!("{}Stream", method.identifier());
        quote!(type ResponseStream = T::#stream)
    } else {
        quote!(type ResponseStream = BoxStream<#response>)
    };

    let inner_arg = if use_arc_self {
        quote!(inner)
    } else {
        quote!(&inner)
    };

    quote! {
        #[allow(non_camel_case_types)]
        struct #service_ident<T: #server_trait>(pub Arc<T>);

        impl<T: #server_trait> tonic::server::StreamingService<#request> for #service_ident<T>
        {
            type Response = #response;
            #response_stream;
            type Future = BoxFuture<tonic::Response<Self::ResponseStream>, tonic::Status>;

            fn call(&mut self, request: tonic::Request<tonic::Streaming<#request>>) -> Self::Future {
                let inner = Arc::clone(&self.0);
                let fut = async move {
                    <T as #server_trait>::#method_ident(#inner_arg, request).await
                };
                Box::pin(fut)
            }
        }

        let accept_compression_encodings = self.accept_compression_encodings;
        let send_compression_encodings = self.send_compression_encodings;
        let max_decoding_message_size = self.max_decoding_message_size;
        let max_encoding_message_size = self.max_encoding_message_size;
        let inner = self.inner.clone();
        let fut = async move {
            let inner = inner.0;
            let method = #service_ident(inner);
            let codec = #codec_name::default();

            let mut grpc = tonic::server::Grpc::new(codec)
                .apply_compression_config(accept_compression_encodings, send_compression_encodings)
                .apply_max_message_size_config(max_decoding_message_size, max_encoding_message_size);

            let res = grpc.streaming(method, req).await;
            Ok(res)
        };

        Box::pin(fut)
    }
}
