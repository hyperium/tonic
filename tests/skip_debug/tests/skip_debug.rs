use std::{fs, path::PathBuf};

#[test]
fn skip_debug() {
    let path = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("test.rs");
    let s = fs::read_to_string(path).unwrap();
    assert!(s.contains("#[prost(skip_debug)]\npub struct Output {}"));
}
