#[doc(hidden)]
#[macro_export]
macro_rules! include_helper {
    ($include: ident, $ending:tt) => {
        include!(concat!(
            env!("OUT_DIR"),
            concat!("/", stringify!($include), $ending)
        ));
    };
}

/// Includes generated proto server and client modules.
///
/// Optionally, a second argument can be provided to rename the included module.
///
/// # Example
/// ```rust,no_run
/// use tonic::include_proto;
///
/// include_proto!(helloworld, hello_world);
/// ```
#[macro_export]
macro_rules! include_proto {
    ($name: ident) => {
        pub mod $name {
            $crate::include_helper!($name, ".rs");
            $crate::include_helper!($name, "_client.rs");
            $crate::include_helper!($name, "_server.rs");
        }
    };
    ($name: ident, $module: ident) => {
        pub mod $module {
            $crate::include_helper!($name, ".rs");
            $crate::include_helper!($name, "_client.rs");
            $crate::include_helper!($name, "_server.rs");
        }
    };
}

/// Include a generated proto client module.
///
/// This shouldn't be used alongside `include_server!` as shared items will conflict.
/// In that case, use `include_proto!` instead.
///
/// Optionally, a second argument can be provided to rename the included module.
///
/// # Example
/// ```rust,no_run
/// use tonic::include_client;
///
/// include_client!(helloworld, hello_world);
/// ```
#[macro_export]
macro_rules! include_client {
    ($name: ident) => {
        pub mod $name {
            $crate::include_helper!($name, ".rs");
            $crate::include_helper!($name, "_client.rs");
        }
    };
    ($name: ident, $module: ident) => {
        pub mod $module {
            $crate::include_helper!($name, ".rs");
            $crate::include_helper!($name, "_client.rs");
        }
    };
}

/// Include a generated proto server module.
///
/// This shouldn't be used alongside `include_client!` as shared items will conflict.
/// In that case, use `include_proto!` instead.
///
/// Optionally, a second argument can be provided to rename the included module.
///
/// # Example
/// ```rust,no_run
/// use tonic::include_server;
///
/// include_server!(helloworld, hello_world);
/// ```
#[macro_export]
macro_rules! include_server {
    ($name: ident) => {
        pub mod $name {
            $crate::include_helper!($name, ".rs");
            $crate::include_helper!($name, "_server.rs");
        }
    };
    ($name: ident, $module: ident) => {
        pub mod $module {
            $crate::include_helper!($name, ".rs");
            $crate::include_helper!($name, "_server.rs");
        }
    };
}
