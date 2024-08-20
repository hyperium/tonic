use std::{fs, path::PathBuf};

#[test]
fn test() {
    let path = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("test.rs");
    let s = fs::read_to_string(path).unwrap();
    assert_eq!(s.match_indices("#[deprecated]").count(), 1);
}
