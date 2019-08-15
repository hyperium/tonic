#![feature(async_await)]
#![recursion_limit = "256"]

extern crate proc_macro;

mod service;

use proc_macro::TokenStream;
use serde::Deserialize;
use syn::{AttributeArgs, ItemImpl};

#[proc_macro_attribute]
pub fn server(attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut original = item.clone();
    let item = syn::parse_macro_input!(item as ItemImpl);
    let args = syn::parse_macro_input!(attr as AttributeArgs);

    let service = load_service(args);
    let service_def = service::parse_service_impl(item, service);
    let output = service::generate(service_def);

    original.extend(TokenStream::from(output));
    original

    // let mut original = item.clone();
    // let ItemImpl { self_ty, items, .. } = syn::parse_macro_input!(item as ItemImpl);

    // let mut m_ident = None;
    // for item in items {
    //     if let ImplItem::Method(method) = item {
    //         // println!("{:?}", method);

    //         let ImplItemMethod { sig, .. } = method;

    //         if sig.asyncness.is_some() {
    //             let name = format!("{}", sig.ident);

    //             if let Some(_method) = service.methods.iter().find(|method| method.name == name) {
    //                 // println!("found method!");
    //                 m_ident = Some(sig.ident.clone());
    //             }
    //         }
    //     }
    // }

    // let service_name = service.proto_name.clone();

    // let ts = quote! {
    //     use tonic::_codegen;
    //     use proto::*;

    //     #[derive(Clone)]
    //     pub struct GrpcServer {
    //         inner: std::sync::Arc<#s>,
    //     }

    //     impl GrpcServer {
    //         fn new(t: #s) -> Self {
    //             Self { inner: std::sync::Arc::new(t) }
    //         }
    //     }

    //     impl _codegen::Service<()> for GrpcServer {
    //         type Response = Self;
    //         type Error = tonic::error::Never;
    //         type Future = _codegen::Ready<Result<Self::Response, Self::Error>>;

    //         fn poll_ready(&mut self, _cx: &mut _codegen::Context<'_>) -> _codegen::Poll<Result<(), Self::Error>> {
    //             std::task::Poll::Ready(Ok(()))
    //         }

    //         fn call(&mut self, _: ()) -> Self::Future {
    //             _codegen::ok(self.clone())
    //         }
    //     }

    //     impl _codegen::Service<_codegen::http::Request<tower_h2::RecvBody>> for GrpcServer {
    //         type Response = _codegen::http::Response<tonic::body::BoxAsyncBody>;
    //         type Error = tonic::error::Never;
    //         type Future = _codegen::ResponseFuture2<Self::Response, Self::Error>;

    //         fn poll_ready(&mut self, _cx: &mut _codegen::Context<'_>) -> _codegen::Poll<Result<(), Self::Error>> {
    //             Ok(()).into()
    //         }

    //         fn call(&mut self, request: _codegen::http::Request<tower_h2::RecvBody>) -> Self::Future {
    //             let inner = self.inner.clone();

    //             match request.uri().path() {
    //                 "/helloworld.Greeter/SayHello" => {
    //                     use tonic::_codegen::*;
    //                     use tonic::*;

    //                     pub struct SayHello(pub std::sync::Arc<#s>);

    //                     impl tonic::server::UnaryService<HelloRequest> for SayHello {
    //                         type Response = HelloReply;
    //                         type Future = Pin<Box<dyn Future<Output = Result<Response<Self::Response>, Status>> + Send + 'static>>;

    //                         fn call(&mut self, request: Request<HelloRequest>) -> Self::Future {
    //                             let inner = self.0.clone();
    //                             let fut = async move {
    //                                 inner.#m_ident(request).await
    //                             };
    //                             Box::pin(fut)
    //                         }
    //                     }

    //                     let inner = self.inner.clone();

    //                     let fut = async move {
    //                         let method = SayHello(inner);
    //                         let codec = tonic::codec::ProstCodec::new();
    //                         let mut grpc = tonic::server::Grpc::new(codec);
    //                         let res = grpc.unary(method, request).await;
    //                         Ok(res)
    //                     };

    //                     Box::pin(fut)
    //                 },
    //                 _ => unimplemented!("use grpc unimplemented")
    //             }
    //         }
    //     }

    // };
}

fn load_service(attr: AttributeArgs) -> Service {
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

    let file = format!(
        "{}/{}.json",
        std::env::var("OUT_DIR").unwrap(),
        service_name
    );
    let json = std::fs::read_to_string(file).unwrap();

    serde_json::from_str(&json).unwrap()
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
