mod add_origin;
pub(crate) use self::add_origin::AddOrigin;

mod connector;
pub(crate) use self::connector::Connector;

mod connection;
pub(crate) use self::connection::Connection;

mod discover;
pub(crate) use self::discover::DynamicServiceStream;

pub(crate) mod executor;
pub(crate) use self::executor::{Executor, SharedExec};

pub(crate) mod io;

mod reconnect;

mod user_agent;
pub(crate) use self::user_agent::UserAgent;

#[cfg(feature = "tls")]
mod tls;
#[cfg(feature = "tls")]
pub(crate) use self::tls::TlsConnector;
