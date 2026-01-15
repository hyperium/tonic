//! Build script for the auxiliary components: protobuf for testing and etc.
//! To invoke, run:
//! ```
//! cargo run -p tonic-xds --bin gen-test-proto --features test-proto-codegen
//! ```
use std::path::PathBuf;
fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let proto_dir = manifest_dir.join("proto/test");
    let proto_file = proto_dir.join("helloworld.proto");
    let out_dir = manifest_dir.join("src/testutil/proto");
    println!("Writing generated test protos to {}", out_dir.display());
    tonic_prost_build::configure()
        .out_dir(proto_dir.clone())
        .compile_protos(
            &[proto_file.to_str().unwrap()],
            &[proto_dir.to_str().unwrap()],
        )
        .unwrap();
}
