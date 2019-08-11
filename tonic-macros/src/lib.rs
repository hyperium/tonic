#![feature(async_await)]

extern crate proc_macro;
use proc_macro::TokenStream;
use prost_build::{Comments, Method, Service};
use quote::quote;
use syn::{ImplItem, ImplItemMethod, ItemImpl, Type};

#[proc_macro_attribute]
pub fn server(attr: TokenStream, item: TokenStream) -> TokenStream {
    let service = load_service(attr);
    let mut original = item.clone();
    let ItemImpl { self_ty, items, .. } = syn::parse_macro_input!(item as ItemImpl);

    let s = if let Type::Path(t) = *self_ty {
        t.path.segments.iter().next().unwrap().clone()
    } else {
        panic!("wrong type!")
    };

    let mut m_ident = None;
    for item in items {
        if let ImplItem::Method(method) = item {
            // println!("{:?}", method);

            let ImplItemMethod { sig, .. } = method;

            if sig.asyncness.is_some() {
                let name = format!("{}", sig.ident);

                if let Some(_method) = service.methods.iter().find(|method| method.name == name) {
                    // println!("found method!");
                    m_ident = Some(sig.ident.clone());
                }
            }
        }
    }

    let ts = quote! {
        pub struct GrpcServer {
            inner: std::sync::Arc<#s>,
        }

        impl From<#s> for GrpcServer {
            fn from(t: #s) -> Self {
                Self { inner: std::sync::Arc::new(t) }
            }
        }

        impl tower_service::Service<tonic::Request<()>> for GrpcServer {
            type Response = tonic::Response<()>;
            type Error = Status;
            type Future = tonic::ResponseFuture<'static, Self::Response>;

            fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
                std::task::Poll::Ready(Ok(()))
            }

            fn call(&mut self, request: tonic::Request<()>) -> tonic::ResponseFuture<'static, Self::Response> {
                let inner = self.inner.clone();
                Box::pin(async move {
                    inner.#m_ident(request).await
                })
            }
        }
    };

    original.extend(TokenStream::from(ts));
    original
}

fn load_service(_attr: TokenStream) -> Service {
    Service {
        name: "Greeter".into(),
        proto_name: "greeter".into(),
        package: "helloworld".into(),
        comments: Comments {
            leading_detached: Vec::new(),
            leading: Vec::new(),
            trailing: Vec::new(),
        },
        methods: vec![Method {
            name: "say_hello".into(),
            proto_name: "SayHello".into(),
            comments: Comments {
                leading_detached: Vec::new(),
                leading: Vec::new(),
                trailing: Vec::new(),
            },
            input_type: "HelloRequest".into(),
            output_type: "HelloResponse".into(),
            input_proto_type: "HelloRequest".into(),
            output_proto_type: "HelloResponse".into(),
            options: Default::default(),
            client_streaming: false,
            server_streaming: false,
        }],
        options: Default::default(),
    }
}
