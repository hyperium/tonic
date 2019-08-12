#![feature(async_await)]
#![recursion_limit = "256"]

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

    // let service_name = service.proto_name.clone();

    let ts = quote! {
        use tonic::_codegen;

        #[derive(Clone)]
        pub struct GrpcServer {
            inner: std::sync::Arc<#s>,
        }

        impl From<#s> for GrpcServer {
            fn from(t: #s) -> Self {
                Self { inner: std::sync::Arc::new(t) }
            }
        }

        impl _codegen::Service<()> for GrpcServer {
            type Response = Self;
            type Error = tonic::error::Never;
            type Future = _codegen::Ready<Result<Self::Response, Self::Error>>;

            fn poll_ready(&mut self, _cx: &mut _codegen::Context<'_>) -> _codegen::Poll<Result<(), Self::Error>> {
                std::task::Poll::Ready(Ok(()))
            }

            fn call(&mut self, _: ()) -> Self::Future {
                _codegen::ok(self.clone())
            }
        }

        impl _codegen::Service<_codegen::http::Request<()>> for GrpcServer {
            type Response = tonic::Response<()>;
            type Error = tonic::error::Never;
            type Future = greeter::ResponseFuture;

            fn poll_ready(&mut self, _cx: &mut _codegen::Context<'_>) -> _codegen::Poll<Result<(), Self::Error>> {
                Ok(()).into()
            }

            fn call(&mut self, request: _codegen::http::Request<()>) -> Self::Future {
                let inner = self.inner.clone();

                match request.uri().path() {
                    "/helloworld.Greeter/SayHello" => {
                        // let kind = greeter::methods::SayHello(self.inner.clone());
                        // greeter::ResponseFuture { kind: greeter::Kind::SayHello(kind) }
                        self.inner.stream(request).await?;
                        unimplemented!()
                    },
                    _ => unimplemented!("use grpc unimplemented")
                }
            }
        }

        // TODO: get actual service name
        pub mod greeter {
            use tonic::_codegen::*;

            pub struct ResponseFuture {
                pub kind: Kind,
            }

            pub enum Kind {
                SayHello(methods::SayHello),
            }

            impl Future for ResponseFuture {
                type Output = Result<tonic::Response<()>, tonic::error::Never>;

                fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
                    unimplemented!()
                }
            }

            pub mod methods {
                use tonic::_codegen::*;

                pub struct SayHello(pub std::sync::Arc<super::super::#s>);

                impl Service<tonic::Request<()>> for SayHello {
                    type Response = tonic::Response<()>;
                    type Error = tonic::Status;
                    type Future = ResponseFuture<Self::Response>;

                    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
                        Ok(()).into()
                    }

                    fn call(&mut self, request: tonic::Request<()>) -> Self::Future {
                        let inner = self.0.clone();

                        Box::pin(async move {
                            inner.#m_ident(request).await
                        })
                    }
                }
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
