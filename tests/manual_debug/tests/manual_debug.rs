use manual_debug::pb::{ManualDebug, DeriveDebug};

#[test]
fn test() {
    let manual = ManualDebug {
        manual: "helloWorld".into(),
    };
    assert_eq!(format!("{manual:?}"), "ManualDebug manual implementation");

    let derived = DeriveDebug {
        derive: "helloWorld".into(),
    };
    assert_eq!(format!("{derived:?}"), r#"DeriveDebug { derive: "helloWorld" }"#
    )
}
