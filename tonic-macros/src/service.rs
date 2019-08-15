use crate::{Method, Service};
use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{Ident, ImplItem, ImplItemMethod, ItemImpl, Lit, LitStr, Path, Type};

#[derive(Debug)]
pub struct ServiceDef {
    name: Path,
    name_str: String,
    package: String,
    proto_name: String,
    proto_path: String,
    methods: Vec<(Method, Ident)>,
}

pub(crate) fn parse_service_impl(
    item: ItemImpl,
    mut service: Service,
    proto_path: String,
) -> ServiceDef {
    let ItemImpl { self_ty, items, .. } = item;

    let name = if let Type::Path(t) = *self_ty {
        t.path.clone()
    } else {
        panic!("wrong type!")
    };

    let mut methods = Vec::new();

    for item in items {
        if let ImplItem::Method(method) = item {
            let ImplItemMethod { sig, .. } = method;

            if sig.asyncness.is_some() {
                let name = format!("{}", sig.ident);

                if let Some((i, _)) = service
                    .methods
                    .iter()
                    .enumerate()
                    .find(|(_, method)| method.name == name)
                {
                    let method = service.methods.remove(i);
                    methods.push((method, sig.ident));
                }
            }
        }
    }

    ServiceDef {
        name,
        name_str: service.name,
        package: service.package,
        proto_name: service.proto_name,
        proto_path,
        methods,
    }
}

pub(crate) fn generate(service: ServiceDef) -> TokenStream {
    let service_server = Ident::new(&format!("{}Server", service.name_str), Span::call_site());

    let service_impl = service.name.clone();
    let methods = generate_methods(&service);

    quote! {
        use tonic::_codegen::*;

        // TODO: impl debug
        #[derive(Clone)]
        pub struct #service_server {
            inner: std::sync::Arc<#service_impl>,
        }

        impl #service_server {
            pub fn new(t: #service_impl) -> Self {
                let inner = std::sync::Arc::new(t);
                Self { inner }
            }
        }

        impl Service<()> for #service_server {
            type Response = Self;
            type Error = tonic::error::Never;
            type Future = Ready<Result<Self::Response, Self::Error>>;

            fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }

            fn call(&mut self, _: ()) -> Self::Future {
                ok(self.clone())
            }
        }

        impl Service<http::Request<tower_h2::RecvBody>> for #service_server {
            type Response = http::Response<tonic::BoxAsyncBody>;
            type Error = tonic::error::Never;
            type Future = BoxFuture<Self::Response, Self::Error>;

            fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }

            fn call(&mut self, req: http::Request<tower_h2::RecvBody>) -> Self::Future {
                let inner = self.inner.clone();

                match req.uri().path() {
                    #methods

                    _ => unimplemented!("use grpc unimplemented"),
                }
            }
        }
    }
}

fn generate_methods(service: &ServiceDef) -> TokenStream {
    let mut stream = TokenStream::new();

    for (method, ident) in &service.methods {
        let path = format!(
            "/{}.{}/{}",
            service.package, service.proto_name, method.proto_name
        );
        let method_path = Lit::Str(LitStr::new(&path, Span::call_site()));

        let method_stream = generate_unary(
            method,
            ident.clone(),
            service.name.clone(),
            &service.proto_path,
        );

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
    service_impl: Path,
    proto_path: &str,
) -> TokenStream {
    let service_ident = Ident::new(&method.proto_name, Span::call_site());

    let request: Path = syn::parse_str(&format!("{}::{}", proto_path, method.input_type)).unwrap();
    let response: Path =
        syn::parse_str(&format!("{}::{}", proto_path, method.output_type)).unwrap();

    quote! {
        struct #service_ident(pub std::sync::Arc<#service_impl>);

        impl tonic::server::UnaryService<#request> for #service_ident {
            type Response =#response;
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

        Box::pin(fut)
    }
}
