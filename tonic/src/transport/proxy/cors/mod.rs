pub mod builder;
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod service;

pub use self::builder::CorsBuilder;
pub use self::config::AllowedOrigins;
pub use self::config::Config;
pub use self::service::CorsService;

pub use self::config::CorsResource;
