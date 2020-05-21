/// Include generated proto server and client items.
///
/// You must specify the gRPC package name.
///
/// ```rust,ignore
/// mod pb {
///     tonic::include_proto!("helloworld");
/// }
/// ```
///
/// # Note:
/// **This only works if the tonic-build output directory has been unmodified**.
/// The default output directory is set to the [`OUT_DIR`] environment variable.
/// If the output directory has been modified, the following pattern may be used
/// instead of this macro.
///
/// ```rust,ignore
/// mod pb {
///     include!("/relative/protobuf/directory/helloworld.rs");
/// }
/// ```
/// You can also use a custom environment variable using the following pattern.
/// ```rust,ignore
/// mod pb {
///     include!(concat!(env!("PROTOBUFS"), "/helloworld.rs"));
/// }
/// ```
///
/// [`OUT_DIR`]: https://doc.rust-lang.org/cargo/reference/environment-variables.html#environment-variables-cargo-sets-for-build-scripts
#[macro_export]
macro_rules! include_proto {
    ($package: tt) => {
        include!(concat!(env!("OUT_DIR"), concat!("/", $package, ".rs")));
    };
}

/// Include encoded `prost_types::FileDescriptorSet` as a `&'static [u8]`.
///
/// ```rust,ignore
/// mod pb {
///     pub(crate) const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!();
/// }
/// ```
///
/// # Note:
/// **This only works if the tonic-build output directory has been unmodified**.
/// The default output directory is set to the [`OUT_DIR`] environment variable.
/// If the output directory has been modified, the following pattern may be used
/// instead of this macro.
///
/// ```rust,ignore
/// mod pb {
///     pub(crate) const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!("/relative/protobuf/directory/file_descriptor_set.bin");
/// }
/// ```
#[macro_export]
macro_rules! include_file_descriptor_set {
    () => {
        include_bytes!(concat!(env!("OUT_DIR"), "/file_descriptor_set.bin"));
    };
}
