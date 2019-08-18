#![feature(async_await)]
#![recursion_limit = "256"]

extern crate proc_macro;

mod client;
mod service;

use proc_macro::TokenStream;
use quote::quote;
use serde::Deserialize;
use syn::{AttributeArgs, ItemImpl};

#[proc_macro]
pub fn client(attr: TokenStream) -> TokenStream {
    let args = syn::parse_macro_input!(attr as AttributeArgs);
    let (service, proto_path) = load_service(args);

    let service_ident = quote::format_ident!("{}Client", service.name);
    let methods = client::generate(service, proto_path);

    let output = quote! {
        pub struct #service_ident <T> {
            inner: tonic::client::Grpc<T>,
        }

        impl<T> #service_ident <T>
        where T: tonic::GrpcService<tonic::body::BoxBody>,
              T::ResponseBody: tonic::body::Body + tonic::_codegen::HttpBody + Send + 'static,
              <T::ResponseBody as tonic::_codegen::HttpBody>::Error: Into<tonic::error::Error> + Send,
              <T::ResponseBody as tonic::_codegen::HttpBody>::Data: Send, {
            pub fn new(inner: T) -> Self {
                let inner = tonic::client::Grpc::new(inner);
                Self { inner }
            }

            #methods
        }

        impl<T: Clone> Clone for #service_ident <T> {
            fn clone(&self) -> Self {
                Self {
                    inner: self.inner.clone(),
                }
            }
        }
    };

    TokenStream::from(output)
}

#[proc_macro_attribute]
pub fn server(attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut original = item.clone();
    let item = syn::parse_macro_input!(item as ItemImpl);
    let args = syn::parse_macro_input!(attr as AttributeArgs);

    let (service, proto_path) = load_service(args);

    let service_def = service::parse_service_impl(item, service, proto_path);
    let output = service::generate(service_def);

    original.extend(TokenStream::from(output));
    original
}

fn load_service(attr: AttributeArgs) -> (Service, String) {
    use syn::{Lit, Meta, MetaNameValue, NestedMeta};

    let service = attr
        .iter()
        .filter_map(|i| match i {
            NestedMeta::Meta(Meta::NameValue(MetaNameValue { path, lit, .. }))
                if path.segments.first().unwrap().ident == "service" =>
            {
                Some(lit.clone())
            }
            _ => None,
        })
        .next();

    let service_name = match service {
        Some(Lit::Str(s)) => s.value(),
        Some(_) => panic!("expected a literal string"),
        None => panic!("expected a `service = \"package.Service\" attribute"),
    };

    let service = attr
        .iter()
        .filter_map(|i| match i {
            NestedMeta::Meta(Meta::NameValue(MetaNameValue { path, lit, .. }))
                if path.segments.first().unwrap().ident == "proto" =>
            {
                Some(lit.clone())
            }
            _ => None,
        })
        .next();

    let proto_path = match service {
        Some(Lit::Str(s)) => s.value(),
        Some(_) => panic!("expected a literal string"),
        None => panic!("expected a `proto = \"my::proto::path\" attribute"),
    };

    let file = format!(
        "{}/{}.json",
        std::env::var("OUT_DIR").unwrap(),
        service_name
    );
    let json = std::fs::read_to_string(file).unwrap();
    let svc = serde_json::from_str(&json).unwrap();

    (svc, proto_path)
}

/// A service descriptor.
#[derive(Debug, Deserialize)]
pub(crate) struct Service {
    /// The service name in Rust style.
    pub name: String,
    /// The service name as it appears in the .proto file.
    pub proto_name: String,
    /// The package name as it appears in the .proto file.
    pub package: String,
    /// The service methods.
    pub methods: Vec<Method>,
}

/// A service method descriptor.
#[derive(Debug, Deserialize)]
pub(crate) struct Method {
    /// The name of the method in Rust style.
    pub name: String,
    /// The name of the method as it appears in the .proto file.
    pub proto_name: String,
    /// The input Rust type.
    pub input_type: String,
    /// The output Rust type.
    pub output_type: String,
    /// The input Protobuf type.
    pub input_proto_type: String,
    /// The output Protobuf type.
    pub output_proto_type: String,
    // /// The method options.
    // pub options: prost_types::MethodOptions,
    /// Identifies if client streams multiple client messages.
    pub client_streaming: bool,
    /// Identifies if server streams multiple server messages.
    pub server_streaming: bool,
}
