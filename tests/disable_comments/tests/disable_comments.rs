use std::{fs, path::PathBuf};

#[test]
fn test() {
    let path = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("test.rs");
    let s = fs::read_to_string(path).unwrap();
    assert!(!s.contains("This comment will be removed."));
    let mut count = 0_usize;
    let mut index = 0_usize;
    while let Some(found) = s[index..].find("This comment will not be removed.") {
        index += found + 1;
        count += 1;
    }
    assert_eq!(count, 2 + 3 + 3); // message: 2, client: 3, server: 3
}
