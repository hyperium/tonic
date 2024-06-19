mod add_origin;
use self::add_origin::AddOrigin;

mod user_agent;
use self::user_agent::UserAgent;

mod reconnect;
use self::reconnect::Reconnect;

mod connection;
pub(super) use self::connection::Connection;

mod discover;
pub(super) use self::discover::DynamicServiceStream;

mod io;
use self::io::BoxedIo;

mod connector;
pub(crate) use self::connector::{ConnectError, Connector};

mod executor;
pub(super) use self::executor::{Executor, SharedExec};

#[cfg(feature = "tls")]
mod tls;
#[cfg(feature = "tls")]
pub(super) use self::tls::TlsConnector;
