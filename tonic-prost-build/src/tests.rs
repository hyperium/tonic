use super::*;
use prost_build::{Comments, Method};

/// Create a test method with separate proto types and resolved rust types.
/// This reflects how prost-build actually works: input_proto_type is the original
/// protobuf type (e.g., ".google.protobuf.BoolValue") while input_type is the
/// resolved Rust type (e.g., "bool").
fn create_test_method_with_proto_types(
    input_type: String,
    output_type: String,
    input_proto_type: String,
    output_proto_type: String,
) -> TonicBuildMethod {
    TonicBuildMethod {
        prost_method: Method {
            name: "TestMethod".to_string(),
            proto_name: "testMethod".to_string(),
            comments: Comments {
                leading: vec![],
                trailing: vec![],
                leading_detached: vec![],
            },
            input_type,
            output_type,
            input_proto_type,
            output_proto_type,
            client_streaming: false,
            server_streaming: false,
            options: prost_types::MethodOptions::default(),
        },
        codec_path: "tonic_prost::ProstCodec".to_string(),
    }
}

/// Legacy helper for non-google types where proto type == rust type
fn create_test_method(input_type: String, output_type: String) -> TonicBuildMethod {
    create_test_method_with_proto_types(
        input_type.clone(),
        output_type.clone(),
        input_type,
        output_type,
    )
}

#[test]
fn test_request_response_name_google_types_not_compiled() {
    // Test Google well-known types when compile_well_known_types is false.
    // Reflect how prost-build resolves types:
    // - proto_type is the original protobuf type (e.g., ".google.protobuf.BoolValue")
    // - rust_type is what prost-build resolves it to (e.g., "bool")
    let test_cases: Vec<(&str, &str, &str)> = vec![
        // (proto_type, rust_type from prost-build, expected output)
        (".google.protobuf.Empty", "()", "()"),
        (
            ".google.protobuf.Any",
            "::prost_types::Any",
            ":: prost_types :: Any",
        ),
        (
            ".google.protobuf.StringValue",
            "::prost::alloc::string::String",
            ":: prost :: alloc :: string :: String",
        ),
        (
            ".google.protobuf.Timestamp",
            "::prost_types::Timestamp",
            ":: prost_types :: Timestamp",
        ),
        (
            ".google.protobuf.Duration",
            "::prost_types::Duration",
            ":: prost_types :: Duration",
        ),
        (
            ".google.protobuf.Value",
            "::prost_types::Value",
            ":: prost_types :: Value",
        ),
        // Wrapper types that map to primitives (the bug fix!)
        (".google.protobuf.BoolValue", "bool", "bool"),
        (".google.protobuf.Int32Value", "i32", "i32"),
        (".google.protobuf.Int64Value", "i64", "i64"),
        (".google.protobuf.UInt32Value", "u32", "u32"),
        (".google.protobuf.UInt64Value", "u64", "u64"),
        (".google.protobuf.FloatValue", "f32", "f32"),
        (".google.protobuf.DoubleValue", "f64", "f64"),
        (
            ".google.protobuf.BytesValue",
            "::prost::alloc::vec::Vec<u8>",
            ":: prost :: alloc :: vec :: Vec < u8 >",
        ),
    ];

    for (proto_type, rust_type, expected) in test_cases {
        let method = create_test_method_with_proto_types(
            rust_type.to_string(),
            rust_type.to_string(),
            proto_type.to_string(),
            proto_type.to_string(),
        );
        let (request, response) = method.request_response_name("super", false);

        assert_eq!(
            request.to_string(),
            expected,
            "Failed for input proto_type: {proto_type}, rust_type: {rust_type}"
        );
        assert_eq!(
            response.to_string(),
            expected,
            "Failed for output proto_type: {proto_type}, rust_type: {rust_type}"
        );
    }
}

#[test]
fn test_request_response_name_google_types_compiled() {
    // Test Google well-known types when compile_well_known_types is true.
    // When compile_well_known_types is true, prost-build doesn't resolve
    // google types to external paths, so input_type == input_proto_type
    // without the leading dot and with proper Rust path format
    let test_cases = vec![
        ".google.protobuf.Empty",
        ".google.protobuf.Any",
        ".google.protobuf.StringValue",
        ".google.protobuf.Timestamp",
        ".google.protobuf.BoolValue",
    ];

    for type_name in test_cases {
        // When compile_well_known_types is true, input_type is a path like
        // "google.protobuf.Empty" (not resolved to "()" or "bool")
        let rust_type = type_name.trim_start_matches('.');
        let method = create_test_method_with_proto_types(
            rust_type.to_string(),
            rust_type.to_string(),
            type_name.to_string(),
            type_name.to_string(),
        );
        let (request, response) = method.request_response_name("super", true);

        // When compile_well_known_types is true, it should use the normal path logic
        let expected_path = format!(
            "super :: google :: protobuf :: {}",
            type_name.trim_start_matches(".google.protobuf.")
        );

        assert_eq!(
            request.to_string(),
            expected_path,
            "Failed for input type: {type_name}"
        );
        assert_eq!(
            response.to_string(),
            expected_path,
            "Failed for output type: {type_name}"
        );
    }
}

#[test]
fn test_request_response_name_non_path_types() {
    // Test types in NON_PATH_TYPE_ALLOWLIST
    let method = create_test_method("()".to_string(), "()".to_string());
    let (request, response) = method.request_response_name("super", false);

    assert_eq!(request.to_string(), "()");
    assert_eq!(response.to_string(), "()");
}

#[test]
fn test_request_response_name_extern_types() {
    // Test extern types that start with :: or crate::
    let test_cases = vec![
        "::my_crate::MyType",
        "crate::module::MyType",
        "::external::lib::Type",
    ];

    for type_name in test_cases {
        let method = create_test_method(type_name.to_string(), type_name.to_string());
        let (request, response) = method.request_response_name("super", false);

        // The parsed TokenStream includes spaces between path segments
        let expected = match type_name {
            "::my_crate::MyType" => ":: my_crate :: MyType",
            "crate::module::MyType" => "crate :: module :: MyType",
            "::external::lib::Type" => ":: external :: lib :: Type",
            _ => panic!("Unknown test case: {type_name}"),
        };

        assert_eq!(
            request.to_string(),
            expected,
            "Failed for input type: {type_name}"
        );
        assert_eq!(
            response.to_string(),
            expected,
            "Failed for output type: {type_name}"
        );
    }
}

#[test]
fn test_request_response_name_regular_protobuf_types() {
    // Test regular protobuf types (with dots)
    let test_cases = vec![
        ("mypackage.MyMessage", "super :: mypackage :: MyMessage"),
        ("com.example.User", "super :: com :: example :: User"),
        (".mypackage.MyMessage", "super :: mypackage :: MyMessage"), // Leading dot
        (
            "nested.package.Message",
            "super :: nested :: package :: Message",
        ),
    ];

    for (input, expected) in test_cases {
        let method = create_test_method(input.to_string(), input.to_string());
        let (request, response) = method.request_response_name("super", false);

        assert_eq!(
            request.to_string(),
            expected,
            "Failed for input type: {input}"
        );
        assert_eq!(
            response.to_string(),
            expected,
            "Failed for output type: {input}"
        );
    }
}

#[test]
fn test_request_response_name_different_proto_paths() {
    // Test with different proto_path values
    let method = create_test_method(
        "mypackage.MyMessage".to_string(),
        "mypackage.MyResponse".to_string(),
    );

    let test_paths = vec!["super", "crate::proto", "crate"];

    for proto_path in test_paths {
        let (request, response) = method.request_response_name(proto_path, false);

        // Handle the case where proto_path contains :: which gets spaced out
        let expected_request = if proto_path.contains("::") {
            format!(
                "{} :: mypackage :: MyMessage",
                proto_path.replace("::", " :: ")
            )
        } else {
            format!("{proto_path} :: mypackage :: MyMessage")
        };
        let expected_response = if proto_path.contains("::") {
            format!(
                "{} :: mypackage :: MyResponse",
                proto_path.replace("::", " :: ")
            )
        } else {
            format!("{proto_path} :: mypackage :: MyResponse")
        };

        assert_eq!(
            request.to_string(),
            expected_request,
            "Failed for proto_path: {proto_path}"
        );
        assert_eq!(
            response.to_string(),
            expected_response,
            "Failed for proto_path: {proto_path}"
        );
    }
}

#[test]
fn test_request_response_name_mixed_types() {
    // Test with google type as request and regular type as response
    let method = create_test_method_with_proto_types(
        "()".to_string(),                     // rust type for Empty
        "mypackage.MyResponse".to_string(),   // rust type for regular message
        ".google.protobuf.Empty".to_string(), // proto type
        ".mypackage.MyResponse".to_string(),  // proto type
    );
    let (request, response) = method.request_response_name("super", false);

    assert_eq!(request.to_string(), "()");
    assert_eq!(response.to_string(), "super :: mypackage :: MyResponse");

    // Test with extern type as request and google type as response
    let method = create_test_method_with_proto_types(
        "::external::Request".to_string(),  // rust type (extern path)
        "::prost_types::Any".to_string(),   // rust type for Any
        ".external.Request".to_string(),    // proto type
        ".google.protobuf.Any".to_string(), // proto type
    );
    let (request, response) = method.request_response_name("super", false);

    assert_eq!(request.to_string(), ":: external :: Request");
    assert_eq!(response.to_string(), ":: prost_types :: Any");

    // Test with BoolValue (primitive wrapper) as response
    let method = create_test_method_with_proto_types(
        "mypackage.MyRequest".to_string(),        // rust type
        "bool".to_string(),                       // rust type for BoolValue
        ".mypackage.MyRequest".to_string(),       // proto type
        ".google.protobuf.BoolValue".to_string(), // proto type
    );
    let (request, response) = method.request_response_name("super", false);

    assert_eq!(request.to_string(), "super :: mypackage :: MyRequest");
    assert_eq!(response.to_string(), "bool");
}

#[test]
fn test_is_google_type() {
    assert!(is_google_type(".google.protobuf.Empty"));
    assert!(is_google_type(".google.protobuf.Any"));
    assert!(is_google_type(".google.protobuf.Timestamp"));

    assert!(!is_google_type("google.protobuf.Empty")); // Missing leading dot
    assert!(!is_google_type(".google.api.Http")); // Not protobuf package
    assert!(!is_google_type("mypackage.Message"));
    assert!(!is_google_type(""));
}

#[test]
fn test_non_path_type_allowlist() {
    // Verify that NON_PATH_TYPE_ALLOWLIST contains expected values
    assert!(NON_PATH_TYPE_ALLOWLIST.contains(&"()"));
    assert_eq!(NON_PATH_TYPE_ALLOWLIST.len(), 1);
}

#[test]
fn test_edge_cases() {
    // Test empty string types - skip this test as empty strings cause parse errors
    // This is an edge case that should be handled at a higher level

    // Test types with multiple dots
    let method = create_test_method("a.b.c.d.Message".to_string(), "x.y.z.Response".to_string());
    let (request, response) = method.request_response_name("super", false);
    assert_eq!(request.to_string(), "super :: a :: b :: c :: d :: Message");
    assert_eq!(response.to_string(), "super :: x :: y :: z :: Response");

    // Test type that ends with () but has a package
    let method = create_test_method("mypackage.()".to_string(), "mypackage.()".to_string());
    let (request, response) = method.request_response_name("super", false);
    assert_eq!(request.to_string(), "mypackage . ()");
    assert_eq!(response.to_string(), "mypackage . ()");
}
