use super::schema;
use proc_macro2::TokenStream;
use prost_build::{Config, Method, Service};
use quote::ToTokens;
use std::path::{Path, PathBuf};

impl<'a> schema::Commentable<'a> for Service {
    type Comment = String;
    type CommentContainer = &'a Vec<Self::Comment>;

    fn comment(&'a self) -> Self::CommentContainer {
        &self.comments.leading
    }
}

pub(crate) struct ProstContext {
    pub(crate) proto_path: String,
}

impl schema::Context for ProstContext {
    fn codec_name(&self) -> &str {
        "tonic::codec::ProstCodec"
    }
}

impl<'a> schema::Service<'a> for Service {
    type Method = Method;
    type MethodContainer = &'a Vec<Self::Method>;
    type Context = ProstContext;

    fn name(&self) -> &str {
        &self.name
    }

    fn package(&self) -> &str {
        &self.package
    }

    fn identifier(&self) -> &str {
        &self.proto_name
    }

    fn methods(&'a self) -> Self::MethodContainer {
        &self.methods
    }
}

impl<'a> schema::Commentable<'a> for Method {
    type Comment = String;
    type CommentContainer = &'a Vec<Self::Comment>;

    fn comment(&'a self) -> Self::CommentContainer {
        &self.comments.leading
    }
}

impl<'a> schema::Method<'a> for Method {
    type Context = ProstContext;

    fn name(&self) -> &str {
        &self.name
    }

    fn identifier(&self) -> &str {
        &self.proto_name
    }

    fn client_streaming(&self) -> bool {
        self.client_streaming
    }

    fn server_streaming(&self) -> bool {
        self.server_streaming
    }

    fn request_response_name(&self, context: &Self::Context) -> (TokenStream, TokenStream) {
        let request = if self.input_proto_type.starts_with(".google.protobuf")
            || self.input_type.starts_with("::")
        {
            self.input_type.parse::<TokenStream>().unwrap()
        } else {
            syn::parse_str::<syn::Path>(&format!("{}::{}", context.proto_path, self.input_type))
                .unwrap()
                .to_token_stream()
        };

        let response = if self.output_proto_type.starts_with(".google.protobuf")
            || self.output_type.starts_with("::")
        {
            self.output_type.parse::<TokenStream>().unwrap()
        } else {
            syn::parse_str::<syn::Path>(&format!("{}::{}", context.proto_path, self.output_type))
                .unwrap()
                .to_token_stream()
        };

        (request, response)
    }
}

use crate::{client, server, Builder};

pub(crate) fn compile<P: AsRef<Path>>(
    builder: Builder,
    out_dir: PathBuf,
    protos: &[P],
    includes: &[P],
) -> std::io::Result<()> {
    let mut config = Config::new();

    config.out_dir(out_dir);
    for (proto_path, rust_path) in builder.extern_path.iter() {
        config.extern_path(proto_path, rust_path);
    }
    for (prost_path, attr) in builder.field_attributes.iter() {
        config.field_attribute(prost_path, attr);
    }
    for (prost_path, attr) in builder.type_attributes.iter() {
        config.type_attribute(prost_path, attr);
    }
    config.service_generator(Box::new(ServiceGenerator::new(builder)));

    config.compile_protos(protos, includes)?;

    Ok(())
}

struct ServiceGenerator {
    builder: Builder,
    clients: TokenStream,
    servers: TokenStream,
}

impl ServiceGenerator {
    fn new(builder: Builder) -> Self {
        ServiceGenerator {
            builder,
            clients: TokenStream::default(),
            servers: TokenStream::default(),
        }
    }
}

impl prost_build::ServiceGenerator for ServiceGenerator {
    fn generate(&mut self, service: prost_build::Service, _buf: &mut String) {
        let context = ProstContext {
            proto_path: String::from("super"),
        };

        if self.builder.build_server {
            let server = server::generate(&service, &context);
            self.servers.extend(server);
        }

        if self.builder.build_client {
            let client = client::generate(&service, &context);
            self.clients.extend(client);
        }
    }

    fn finalize(&mut self, buf: &mut String) {
        if self.builder.build_client && !self.clients.is_empty() {
            let clients = &self.clients;

            let client_service = quote::quote! {
                #clients
            };

            let code = format!("{}", client_service);
            buf.push_str(&code);

            self.clients = TokenStream::default();
        }

        if self.builder.build_server && !self.servers.is_empty() {
            let servers = &self.servers;

            let server_service = quote::quote! {
                #servers
            };

            let code = format!("{}", server_service);
            buf.push_str(&code);

            self.servers = TokenStream::default();
        }
    }
}
