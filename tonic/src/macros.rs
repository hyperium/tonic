/// Include generated proto server and client items.
///
/// You must specify the gRPC package name.
#[macro_export]
macro_rules! include_proto {
    ($package: tt) => {
        include!(concat!(env!("OUT_DIR"), concat!("/", $package, ".rs")));
    };
}
