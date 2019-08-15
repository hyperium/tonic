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
        use proto::*;

        #[derive(Clone)]
        pub struct GrpcServer {
            inner: std::sync::Arc<#s>,
        }

        impl GrpcServer {
            fn new(t: #s) -> Self {
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

        impl _codegen::Service<_codegen::http::Request<tower_h2::RecvBody>> for GrpcServer {
            type Response = _codegen::http::Response<tonic::body::BoxAsyncBody>;
            type Error = tonic::error::Never;
            type Future = _codegen::ResponseFuture2<Self::Response, Self::Error>;

            fn poll_ready(&mut self, _cx: &mut _codegen::Context<'_>) -> _codegen::Poll<Result<(), Self::Error>> {
                Ok(()).into()
            }

            fn call(&mut self, request: _codegen::http::Request<tower_h2::RecvBody>) -> Self::Future {
                let inner = self.inner.clone();

                match request.uri().path() {
                    "/helloworld.Greeter/SayHello" => {
                        use tonic::_codegen::*;
                        use tonic::*;

                        pub struct SayHello(pub std::sync::Arc<#s>);

                        impl tonic::server::UnaryService<HelloRequest> for SayHello {
                            type Response = HelloReply;
                            type Future = Pin<Box<dyn Future<Output = Result<Response<Self::Response>, Status>> + Send + 'static>>;

                            fn call(&mut self, request: Request<HelloRequest>) -> Self::Future {
                                let inner = self.0.clone();
                                let fut = async move {
                                    inner.#m_ident(request).await
                                };
                                Box::pin(fut)
                            }
                        }

                        let inner = self.inner.clone();


                        let fut = async move {
                            let method = SayHello(inner);
                            let codec = tonic::codec::ProstCodec::new();
                            let mut grpc = tonic::server::Grpc::new(codec);
                            let res = grpc.unary(method, request).await;
                            Ok(res)
                        };

                        Box::pin(fut)
                    },
                    _ => unimplemented!("use grpc unimplemented")
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
