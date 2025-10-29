#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/*.rs");
}
