#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Animal {
    #[prost(string, optional, tag = "1")]
    pub name: ::core::option::Option<::prost::alloc::string::String>,
}

// pub mod foo;

pub mod foo {
    pub mod bar {
        pub mod baz {
            tonic::include_proto!("foo.bar.baz");
        }
    }
}

fn main() {
    println!("Hello, world!");
}
