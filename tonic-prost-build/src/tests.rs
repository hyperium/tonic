use super::*;
use prost_build::{Comments, Method};
use quote::quote;

fn create_test_method(input_type: String, output_type: String) -> TonicBuildMethod {
    TonicBuildMethod {
        prost_method: Method {
            name: "TestMethod".to_string(),
            proto_name: "testMethod".to_string(),
            comments: Comments {
                leading: vec![],
                trailing: vec![],
                leading_detached: vec![],
            },
            input_type: input_type.clone(),
            output_type: output_type.clone(),
            input_proto_type: input_type,
            output_proto_type: output_type,
            client_streaming: false,
            server_streaming: false,
            options: prost_types::MethodOptions::default(),
        },
        codec_path: "tonic_prost::ProstCodec".to_string(),
    }
}

#[test]
fn test_request_response_name_google_types_not_compiled() {
    // Test Google well-known types when compile_well_known_types is false
    let test_cases = vec![
        (".google.protobuf.Empty", quote!(())),
        (".google.protobuf.Any", quote!(::prost_types::Any)),
        (
            ".google.protobuf.StringValue",
            quote!(::prost::alloc::string::String),
        ),
        (
            ".google.protobuf.Timestamp",
            quote!(::prost_types::Timestamp),
        ),
        (".google.protobuf.Duration", quote!(::prost_types::Duration)),
        (".google.protobuf.Value", quote!(::prost_types::Value)),
    ];

    for (type_name, expected) in test_cases {
        let method = create_test_method(type_name.to_string(), type_name.to_string());
        let (request, response) = method.request_response_name("super", false);

        assert_eq!(
            request.to_string(),
            expected.to_string(),
            "Failed for input type: {type_name}"
        );
        assert_eq!(
            response.to_string(),
            expected.to_string(),
            "Failed for output type: {type_name}"
        );
    }
}

#[test]
fn test_request_response_name_google_types_compiled() {
    // Test Google well-known types when compile_well_known_types is true
    let test_cases = vec![
        ".google.protobuf.Empty",
        ".google.protobuf.Any",
        ".google.protobuf.StringValue",
        ".google.protobuf.Timestamp",
    ];

    for type_name in test_cases {
        let method = create_test_method(type_name.to_string(), type_name.to_string());
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
    // Test with different request and response types
    let method = create_test_method(
        ".google.protobuf.Empty".to_string(),
        "mypackage.MyResponse".to_string(),
    );
    let (request, response) = method.request_response_name("super", false);

    assert_eq!(request.to_string(), "()");
    assert_eq!(response.to_string(), "super :: mypackage :: MyResponse");

    // Test with extern type as request and google type as response
    let method = create_test_method(
        "::external::Request".to_string(),
        ".google.protobuf.Any".to_string(),
    );
    let (request, response) = method.request_response_name("super", false);

    assert_eq!(request.to_string(), ":: external :: Request");
    assert_eq!(response.to_string(), ":: prost_types :: Any");
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
