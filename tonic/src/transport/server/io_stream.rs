use std::{io, ops::ControlFlow, pin::pin};

use tokio::io::{AsyncRead, AsyncWrite};
use tokio_stream::{Stream, StreamExt as _};

use super::service::ServerIo;
#[cfg(feature = "_tls-any")]
use super::service::TlsAcceptor;

#[cfg(not(feature = "_tls-any"))]
pub(crate) fn tcp_incoming<IO, IE>(
    incoming: impl Stream<Item = Result<IO, IE>>,
) -> impl Stream<Item = Result<ServerIo<IO>, crate::BoxError>>
where
    IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    IE: Into<crate::BoxError>,
{
    async_stream::try_stream! {
        let mut incoming = pin!(incoming);

        while let Some(item) = incoming.next().await {
            yield match item {
                Ok(_) => item.map(ServerIo::new_io)?,
                Err(e) => match handle_tcp_accept_error(e) {
                    ControlFlow::Continue(()) => continue,
                    ControlFlow::Break(e) => Err(e)?,
                }
            }
        }
    }
}

#[cfg(feature = "_tls-any")]
pub(crate) fn tcp_incoming<IO, IE>(
    incoming: impl Stream<Item = Result<IO, IE>>,
    tls: Option<TlsAcceptor>,
) -> impl Stream<Item = Result<ServerIo<IO>, crate::BoxError>>
where
    IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    IE: Into<crate::BoxError>,
{
    async_stream::try_stream! {
        let mut incoming = pin!(incoming);

        let mut tasks = tokio::task::JoinSet::new();

        loop {
            match select(&mut incoming, &mut tasks).await {
                SelectOutput::Incoming(stream) => {
                    if let Some(tls) = &tls {
                        let tls = tls.clone();
                        tasks.spawn(async move {
                            let io = tls.accept(stream).await?;
                            Ok(ServerIo::new_tls_io(io))
                        });
                    } else {
                        yield ServerIo::new_io(stream);
                    }
                }

                SelectOutput::Io(io) => {
                    yield io;
                }

                SelectOutput::TcpErr(e) => match handle_tcp_accept_error(e) {
                    ControlFlow::Continue(()) => continue,
                    ControlFlow::Break(e) => Err(e)?,
                }

                SelectOutput::TlsErr(e) => {
                    tracing::debug!(error = %e, "tls accept error");
                    continue;
                }

                SelectOutput::Done => {
                    break;
                }
            }
        }
    }
}

fn handle_tcp_accept_error(e: impl Into<crate::BoxError>) -> ControlFlow<crate::BoxError> {
    let e = e.into();
    tracing::debug!(error = %e, "accept loop error");
    if let Some(e) = e.downcast_ref::<io::Error>() {
        if matches!(
            e.kind(),
            io::ErrorKind::ConnectionAborted
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::BrokenPipe
                | io::ErrorKind::Interrupted
                | io::ErrorKind::WouldBlock
                | io::ErrorKind::TimedOut
        ) {
            return ControlFlow::Continue(());
        }
    }

    ControlFlow::Break(e)
}

#[cfg(feature = "_tls-any")]
async fn select<IO: 'static, IE>(
    incoming: &mut (impl Stream<Item = Result<IO, IE>> + Unpin),
    tasks: &mut tokio::task::JoinSet<Result<ServerIo<IO>, crate::BoxError>>,
) -> SelectOutput<IO>
where
    IE: Into<crate::BoxError>,
{
    if tasks.is_empty() {
        return match incoming.try_next().await {
            Ok(Some(stream)) => SelectOutput::Incoming(stream),
            Ok(None) => SelectOutput::Done,
            Err(e) => SelectOutput::TcpErr(e.into()),
        };
    }

    tokio::select! {
        stream = incoming.try_next() => {
            match stream {
                Ok(Some(stream)) => SelectOutput::Incoming(stream),
                Ok(None) => SelectOutput::Done,
                Err(e) => SelectOutput::TcpErr(e.into()),
            }
        }

        accept = tasks.join_next() => {
            match accept.expect("JoinSet should never end") {
                Ok(Ok(io)) => SelectOutput::Io(io),
                Ok(Err(e)) => SelectOutput::TlsErr(e),
                Err(e) => SelectOutput::TlsErr(e.into()),
            }
        }
    }
}

#[cfg(feature = "_tls-any")]
enum SelectOutput<A> {
    Incoming(A),
    Io(ServerIo<A>),
    TcpErr(crate::BoxError),
    TlsErr(crate::BoxError),
    Done,
}
