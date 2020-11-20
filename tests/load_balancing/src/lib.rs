pub mod lookup;
pub mod test;

pub mod pb {
    tonic::include_proto!("test");
}

pub use test::*;
