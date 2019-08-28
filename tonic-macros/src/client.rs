use super::{Method, Service};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::Path;

pub(crate) fn generate(service: Service, proto: String) -> TokenStream {
    let mut stream = TokenStream::new();

    for method in &service.methods {
        let path = format!(
            "/{}.{}/{}",
            service.package, service.proto_name, method.proto_name
        );

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
    let request: Path = syn::parse_str(&format!("{}::{}", proto, method.input_type)).unwrap();
    let response: Path = syn::parse_str(&format!("{}::{}", proto, method.output_type)).unwrap();

    quote! {
        pub async fn #ident (&mut self, request: tonic::Request<#request>)
            -> Result<tonic::Response<#response>, tonic::Status> {
           self.inner.ready().await?;
           let codec = tonic::codec::ProstCodec::new();
           let path = http::uri::PathAndQuery::from_static(#path);
           self.inner.unary(request, path, codec).await
        }
    }
}

fn generate_server_streaming(method: &Method, proto: &str, path: String) -> TokenStream {
    let ident = format_ident!("{}", method.name);
    let request: Path = syn::parse_str(&format!("{}::{}", proto, method.input_type)).unwrap();
    let response: Path = syn::parse_str(&format!("{}::{}", proto, method.output_type)).unwrap();

    quote! {
        pub async fn #ident (&mut self, request: tonic::Request<#request>)
            -> Result<tonic::Response<tonic::codec::Streaming<#response>>, tonic::Status> {
           self.inner.ready().await?;
           let codec = tonic::codec::ProstCodec::new();
           let path = http::uri::PathAndQuery::from_static(#path);
           self.inner.server_streaming(request, path, codec).await
        }
    }
}

fn generate_client_streaming(method: &Method, proto: &str, path: String) -> TokenStream {
    let ident = format_ident!("{}", method.name);
    let request: Path = syn::parse_str(&format!("{}::{}", proto, method.input_type)).unwrap();
    let response: Path = syn::parse_str(&format!("{}::{}", proto, method.output_type)).unwrap();

    quote! {
        pub async fn #ident <S>(&mut self, request: tonic::Request<S>)
            -> Result<tonic::Response<#response>, tonic::Status>
            where S: tonic::_codegen::Stream<Item = Result<#request, tonic::Status>> + Send + 'static,
        {
           self.inner.ready().await?;
           let codec = tonic::codec::ProstCodec::new();
           let path = http::uri::PathAndQuery::from_static(#path);
           let request = request.map(|s| Box::pin(s));
           self.inner.client_streaming(request, path, codec).await
        }
    }
}

fn generate_streaming(method: &Method, proto: &str, path: String) -> TokenStream {
    let ident = format_ident!("{}", method.name);
    let request: Path = syn::parse_str(&format!("{}::{}", proto, method.input_type)).unwrap();
    let response: Path = syn::parse_str(&format!("{}::{}", proto, method.output_type)).unwrap();

    quote! {
        pub async fn #ident <S>(&mut self, request: tonic::Request<S>)
            -> Result<tonic::Response<tonic::codec::Streaming<#response>>, tonic::Status>
            where S: tonic::_codegen::Stream<Item = Result<#request, tonic::Status>> + Send + 'static,
        {
           self.inner.ready().await?;
           let codec = tonic::codec::ProstCodec::new();
           let path = http::uri::PathAndQuery::from_static(#path);
           let request = request.map(|s| Box::pin(s));
           self.inner.streaming(request, path, codec).await
        }
    }
}
