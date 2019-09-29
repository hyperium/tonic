#[doc(hidden)]
#[macro_export]
macro_rules! include_helper {
    ($include: ident, $ending: literal) => {
        include!(concat!(
            env!("OUT_DIR"),
            concat!("/", stringify!($include), $ending)
        ));
    };
}

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
