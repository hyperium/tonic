use crate::generate_doc_comments;
use proc_macro2::TokenStream;
use prost_build::{Method, Service};
use quote::{format_ident, quote};

pub(crate) fn generate(service: &Service, proto: &str) -> TokenStream {
    let service_ident = quote::format_ident!("{}Client", service.name);
    let methods = generate_methods(service, proto);

    let connect = generate_connect(&service_ident);
    let service_doc = generate_doc_comments(&service.comments.leading);

    quote! {
        #service_doc
        pub struct #service_ident<T> {
            inner: tonic::client::Grpc<T>,
        }

        #connect

        impl<T> #service_ident<T>
        where T: tonic::client::GrpcService<tonic::body::BoxBody>,
              T::ResponseBody: Body + HttpBody + Send + 'static,
              T::Error: Into<StdError>,
              <T::ResponseBody as HttpBody>::Error: Into<StdError> + Send,
              <T::ResponseBody as HttpBody>::Data: Into<bytes::Bytes> + Send, {
            pub fn new(inner: T) -> Self {
                let inner = tonic::client::Grpc::new(inner);
                Self { inner }
            }

            /// Check if the service is ready.
            pub async fn ready(&mut self) -> Result<(), tonic::Status> {
                self.inner.ready().await.map_err(|e| {
                    tonic::Status::new(tonic::Code::Unknown, format!("Service was not ready: {}", e.into()))
                })
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

#[cfg(feature = "transport")]
fn generate_connect(service_ident: &syn::Ident) -> TokenStream {
    quote! {
        impl #service_ident<tonic::transport::Channel> {
            /// Attempt to create a new client by connecting to a given endpoint.
            pub fn connect<D>(dst: D) -> Result<Self, tonic::transport::Error>
            where
                D: std::convert::TryInto<tonic::transport::Endpoint>,
                D::Error: Into<StdError>,
            {
                tonic::transport::Endpoint::new(dst).map(|c| Self::new(c.channel()))
            }
        }
    }
}

#[cfg(not(feature = "transport"))]
fn generate_connect(_service_ident: &syn::Ident) -> TokenStream {
    TokenStream::new()
}

fn generate_methods(service: &Service, proto: &str) -> TokenStream {
    let mut stream = TokenStream::new();

    for method in &service.methods {
        let path = format!(
            "/{}.{}/{}",
            service.package, service.proto_name, method.proto_name
        );

        stream.extend(generate_doc_comments(&method.comments.leading));

        let method = match (method.client_streaming, method.server_streaming) {
            (false, false) => generate_unary(method, &proto, path),
            (false, true) => generate_server_streaming(method, &proto, path),
            (true, false) => generate_client_streaming(method, &proto, path),
            (true, true) => generate_streaming(method, &proto, path),
        };

        stream.extend(method);
    }

    stream
}

fn generate_unary(method: &Method, proto: &str, path: String) -> TokenStream {
    let ident = format_ident!("{}", method.name);
    let (request, response) = crate::replace_wellknown(proto, &method);

    quote! {
        pub async fn #ident(&mut self, request: tonic::Request<#request>)
            -> Result<tonic::Response<#response>, tonic::Status> {
           self.ready().await?;
           let codec = tonic::codec::ProstCodec::new();
           let path = http::uri::PathAndQuery::from_static(#path);
           self.inner.unary(request, path, codec).await
        }
    }
}

fn generate_server_streaming(method: &Method, proto: &str, path: String) -> TokenStream {
    let ident = format_ident!("{}", method.name);

    let (request, response) = crate::replace_wellknown(proto, &method);

    quote! {
        pub async fn #ident(&mut self, request: tonic::Request<#request>)
            -> Result<tonic::Response<tonic::codec::Streaming<#response>>, tonic::Status> {
           self.ready().await?;
           let codec = tonic::codec::ProstCodec::new();
           let path = http::uri::PathAndQuery::from_static(#path);
           self.inner.server_streaming(request, path, codec).await
        }
    }
}

fn generate_client_streaming(method: &Method, proto: &str, path: String) -> TokenStream {
    let ident = format_ident!("{}", method.name);

    let (request, response) = crate::replace_wellknown(proto, &method);

    quote! {
        pub async fn #ident<S>(&mut self, request: tonic::Request<S>)
            -> Result<tonic::Response<#response>, tonic::Status>
            where S: Stream<Item = #request> + Send + 'static,
        {
           self.ready().await?;
           let codec = tonic::codec::ProstCodec::new();
           let path = http::uri::PathAndQuery::from_static(#path);
           self.inner.client_streaming(request, path, codec).await
        }
    }
}

fn generate_streaming(method: &Method, proto: &str, path: String) -> TokenStream {
    let ident = format_ident!("{}", method.name);

    let (request, response) = crate::replace_wellknown(proto, &method);

    quote! {
        pub async fn #ident<S>(&mut self, request: tonic::Request<S>)
            -> Result<tonic::Response<tonic::codec::Streaming<#response>>, tonic::Status>
            where S: Stream<Item = #request> + Send + 'static,
        {
           self.ready().await?;
           let codec = tonic::codec::ProstCodec::new();
           let path = http::uri::PathAndQuery::from_static(#path);
           self.inner.streaming(request, path, codec).await
        }
    }
}
