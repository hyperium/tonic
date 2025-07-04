/// Include generated proto server and client items.
///
/// You must specify the path of the proto file within the proto directory,
/// without the ".proto" extension.
///
/// ```rust,ignore
/// mod pb {
///     grpc::include_proto!("protos", "helloworld");
/// }
/// ```
///
/// # Note:
/// **This only works if the grpc-build output directory and the message path
/// is unmodified**.
/// The default output directory is set to the [`OUT_DIR`] environment variable
/// and the message path is set to `self`.
/// If the output directory has been modified, the following pattern may be used
/// instead of this macro.
///
/// If the message path is `self`.
/// ```rust,ignore
/// mod protos {
///     // Include message code.
///     include!("/relative/protobuf/directory/protos/generated.rs");
///     /// Include service code.
///     include!("/relative/protobuf/directory/proto/helloworld_grpc.pb.rs");
/// }
///```
///
/// If the message code is not in the same module. The following example uses
/// message path as `super::protos`.
/// ```rust,ignore
/// mod protos {
///     // Include message code.
///     include!("/relative/protobuf/directory/protos/generated.rs");
/// }
///
/// mod grpc {
///     /// Include service code.
///     include!("/relative/protobuf/directory/proto/helloworld_grpc.pb.rs");
/// }
/// ```
/// [`OUT_DIR`]: https://doc.rust-lang.org/cargo/reference/environment-variables.html#environment-variables-cargo-sets-for-build-scripts
#[macro_export]
macro_rules! include_proto {
    ($parent_dir:literal, $proto_file:literal) => {
        include!(concat!(env!("OUT_DIR"), "/", $parent_dir, "/generated.rs"));
        include!(concat!(
            env!("OUT_DIR"),
            "/",
            $parent_dir,
            "/",
            $proto_file,
            "_grpc.pb.rs"
        ));
    };
}
