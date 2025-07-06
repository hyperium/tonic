cc_binary(
    name = "protoc-gen-rust-grpc",
    srcs = [
    "grpc_rust_plugin.cc",
    "grpc_rust_generator.h",
    "grpc_rust_generator.cc",
    ],
    visibility = ["//visibility:public"],
    deps = [
        "@com_google_protobuf//:protoc_lib",
    ],
)
