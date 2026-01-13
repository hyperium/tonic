//! Build script for the auxiliary components: protobuf for testing and etc.
#![allow(clippy::unwrap_used)]
use std::{env, path::PathBuf};
fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    tonic_prost_build::configure()
        .file_descriptor_set_path(out_dir.join("helloworld.bin"))
        .compile_protos(&["proto/test/helloworld.proto"], &["proto/test"])
        .unwrap();
}
