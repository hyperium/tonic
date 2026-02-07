#[derive(Clone, PartialEq, ::prost::Message)]
struct Animal {
    #[prost(string, optional, tag = "1")]
    pub name: ::core::option::Option<::prost::alloc::string::String>,
}

mod pb {
    tonic::include_proto!("root_crate_path");
}

fn main() {}
