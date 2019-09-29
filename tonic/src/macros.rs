#[doc(hidden)]
#[macro_export]
macro_rules! include_helper {
    ($include: tt, $ending: tt) => {
        include!(concat!(
            env!("OUT_DIR"),
            concat!("/", $include, $ending)
        ));
    };
}

/// Includes generated proto server and client items.
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
        $crate::include_helper!($name, ".rs");
        $crate::include_helper!($name, "_client.rs");
        $crate::include_helper!($name, "_server.rs");
    };
}

/// Include a generated proto client items.
///
/// This shouldn't be used alongside `include_server!` as shared items will conflict.
/// In that case, use `include_proto!` instead.
///
/// # Example
/// ```rust,no_run
/// pub mod hello_world {
///    tonic::include_client!("helloworld");
/// }
/// ```
#[macro_export]
macro_rules! include_client {
    ($name: tt) => {
        $crate::include_helper!($name, ".rs");
        $crate::include_helper!($name, "_client.rs");
    };
}

/// Include a generated proto server items.
///
/// This shouldn't be used alongside `include_client!` as shared items will conflict.
/// In that case, use `include_proto!` instead.
///
/// # Example
/// ```rust,no_run
/// pub mod hello_world {
///    tonic::include_server!("helloworld");
/// }
/// ```
#[macro_export]
macro_rules! include_server {
    ($name: tt) => {
        $crate::include_helper!($name, ".rs");
        $crate::include_helper!($name, "_server.rs");
    };
}
