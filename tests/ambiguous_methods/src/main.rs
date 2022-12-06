#![allow(clippy::derive_partial_eq_without_eq)]
#[macro_use]
extern crate tonic;

tonic::include_proto!("ambiguous_methods");

fn main() {
    println!("Hello, world!");
}
