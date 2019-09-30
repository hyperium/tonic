/// Include generated proto server and client items.
///
/// # Example
/// ```rust,no_run
/// pub mod hello_world {
///    tonic::include_proto!("helloworld");
/// }
/// ```
#[macro_export]
macro_rules! include_proto {
    ($name: tt) => {
        include!(concat!(
            env!("OUT_DIR"),
            concat!("/", $name, ".rs")
        ));
    };
}