use super::Connected;

/// Connection info for VSOCK socket streams.
///
/// See [Connected] for more details.
///
/// [Connected]: crate::transport::server::Connected
#[cfg_attr(docsrs, doc(cfg(unix)))]
#[derive(Clone, Debug)]
pub struct VsockConnectInfo {
    /// Local address
    pub local_addr: Option<tokio_vsock::VsockAddr>,
    /// Peer address
    pub peer_addr: Option<tokio_vsock::VsockAddr>,
}

impl Connected for tokio_vsock::VsockStream {
    type ConnectInfo = VsockConnectInfo;

    fn connect_info(&self) -> Self::ConnectInfo {
        VsockConnectInfo {
            local_addr: self.local_addr().ok(),
            peer_addr: self.peer_addr().ok(),
        }
    }
}
