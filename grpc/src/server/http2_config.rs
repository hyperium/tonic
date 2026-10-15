/*
 *
 * Copyright 2026 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

use std::time::Duration;

/// Largest HTTP/2 flow-control window size (RFC 9113 §6.9.1).
const MAX_WINDOW_SIZE: u32 = (1 << 31) - 1;

/// Configuration for the HTTP/2 transport.
///
/// All fields default to `None`, which means the transport default is used.
/// Use the fluent setter methods to override specific settings.
#[derive(Debug, Clone, Default)]
pub struct Http2Config {
    max_recv_message_size: Option<usize>,
    max_concurrent_streams: Option<u32>,
    initial_connection_window_size: Option<u32>,
    initial_stream_window_size: Option<u32>,
    keep_alive_interval: Option<Duration>,
    keep_alive_timeout: Option<Duration>,
    max_connection_age: Option<Duration>,
    max_connection_age_grace: Option<Duration>,
    handshake_timeout: Option<Duration>,
}

impl Http2Config {
    /// Creates a new `Http2Config` with default settings (all `None`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the maximum allowed request message payload size in bytes.
    /// If not set, the limit is 4 MiB.
    pub fn with_max_recv_message_size(mut self, size: usize) -> Self {
        self.max_recv_message_size = Some(size);
        self
    }

    /// Sets the maximum number of concurrent HTTP/2 streams per connection.
    /// If not set, the number of concurrent streams is unlimited.
    pub fn with_max_concurrent_streams(mut self, max: u32) -> Self {
        self.max_concurrent_streams = Some(max);
        self
    }

    /// Sets the initial HTTP/2 connection-level flow control window size, and
    /// turns off dynamic flow control. Values above 2^31 - 1 are ignored.
    ///
    /// If neither window size is set, both start at 64 KiB and grow with the
    /// connection's measured bandwidth-delay product. Setting this also stops
    /// the stream-level window from growing: it stays at 64 KiB unless set with
    /// [`Http2Config::with_initial_stream_window_size`].
    pub fn with_initial_connection_window_size(mut self, size: u32) -> Self {
        if size <= MAX_WINDOW_SIZE {
            self.initial_connection_window_size = Some(size);
        }
        self
    }

    /// Sets the initial HTTP/2 stream-level flow control window size, and
    /// turns off dynamic flow control. Values above 2^31 - 1 are ignored.
    ///
    /// If neither window size is set, both start at 64 KiB and grow with the
    /// connection's measured bandwidth-delay product. Setting this also stops
    /// the connection-level window from growing: it stays at 64 KiB unless set
    /// with [`Http2Config::with_initial_connection_window_size`].
    pub fn with_initial_stream_window_size(mut self, size: u32) -> Self {
        if size <= MAX_WINDOW_SIZE {
            self.initial_stream_window_size = Some(size);
        }
        self
    }

    /// Sets the HTTP/2 keep-alive interval and timeout.
    ///
    /// The server sends a PING frame after `interval` of inactivity. If
    /// no response is received within `timeout`, the connection is closed.
    /// If not set, the interval is 2 hours and the timeout is 20 seconds.
    pub fn with_keep_alive(mut self, interval: Duration, timeout: Duration) -> Self {
        self.keep_alive_interval = Some(interval);
        self.keep_alive_timeout = Some(timeout);
        self
    }

    /// Sets the maximum duration a connection is allowed to exist.
    ///
    /// After this duration, the server sends a GOAWAY frame to the client.
    /// Each connection's age is jittered by ±10% (gRFC A9) so that connections
    /// accepted together don't all reconnect at once. In-flight RPCs then get
    /// `max_connection_age_grace` to complete before the connection is
    /// force-closed; without a grace period they may run indefinitely.
    /// If not set, connections have no maximum age.
    pub fn with_max_connection_age(mut self, age: Duration) -> Self {
        self.max_connection_age = Some(age);
        self
    }

    /// Sets the grace period after `max_connection_age` expires.
    ///
    /// After GOAWAY is sent, existing RPCs are given this much additional
    /// time to complete. After the grace period, the connection is forcibly
    /// terminated. Has no effect without `max_connection_age`. If not set,
    /// the grace period is infinite: in-flight RPCs may run indefinitely after
    /// the GOAWAY.
    pub fn with_max_connection_age_grace(mut self, grace: Duration) -> Self {
        self.max_connection_age_grace = Some(grace);
        self
    }

    /// Sets the maximum duration allowed for the server credential handshake
    /// (e.g. TLS) to complete.
    ///
    /// Connections that have not finished handshaking when this elapses are
    /// terminated. If not set, the timeout is 120 seconds.
    pub fn with_handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = Some(timeout);
        self
    }

    /// Returns the maximum request message size, or `None` if unset.
    pub fn get_max_recv_message_size(&self) -> Option<usize> {
        self.max_recv_message_size
    }

    /// Returns the maximum number of concurrent streams, or `None` if unset.
    pub fn get_max_concurrent_streams(&self) -> Option<u32> {
        self.max_concurrent_streams
    }

    /// Returns the initial connection-level window size, or `None` if unset.
    pub fn get_initial_connection_window_size(&self) -> Option<u32> {
        self.initial_connection_window_size
    }

    /// Returns the initial stream-level window size, or `None` if unset.
    pub fn get_initial_stream_window_size(&self) -> Option<u32> {
        self.initial_stream_window_size
    }

    /// Returns the keep-alive ping interval, or `None` if unset.
    pub fn get_keep_alive_interval(&self) -> Option<Duration> {
        self.keep_alive_interval
    }

    /// Returns the keep-alive ping timeout, or `None` if unset.
    pub fn get_keep_alive_timeout(&self) -> Option<Duration> {
        self.keep_alive_timeout
    }

    /// Returns the maximum connection age, or `None` if unset.
    pub fn get_max_connection_age(&self) -> Option<Duration> {
        self.max_connection_age
    }

    /// Returns the grace period after the maximum connection age, or `None` if unset.
    pub fn get_max_connection_age_grace(&self) -> Option<Duration> {
        self.max_connection_age_grace
    }

    /// Returns the credential handshake timeout, or `None` if unset.
    pub fn get_handshake_timeout(&self) -> Option<Duration> {
        self.handshake_timeout
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http2_config_default() {
        let default_cfg = Http2Config::default();
        assert_eq!(default_cfg.get_max_recv_message_size(), None);
        assert_eq!(default_cfg.get_max_concurrent_streams(), None);
        assert_eq!(default_cfg.get_initial_connection_window_size(), None);
        assert_eq!(default_cfg.get_initial_stream_window_size(), None);
        assert_eq!(default_cfg.get_keep_alive_interval(), None);
        assert_eq!(default_cfg.get_keep_alive_timeout(), None);
        assert_eq!(default_cfg.get_max_connection_age(), None);
        assert_eq!(default_cfg.get_max_connection_age_grace(), None);
        assert_eq!(default_cfg.get_handshake_timeout(), None);
    }

    #[test]
    fn http2_config_setters_and_getters() {
        let cfg = Http2Config::new()
            .with_max_recv_message_size(8 * 1024 * 1024)
            .with_max_concurrent_streams(100)
            .with_initial_connection_window_size(1024 * 1024)
            .with_initial_stream_window_size(512 * 1024)
            .with_keep_alive(Duration::from_secs(30), Duration::from_secs(10))
            .with_max_connection_age(Duration::from_secs(300))
            .with_max_connection_age_grace(Duration::from_secs(15))
            .with_handshake_timeout(Duration::from_secs(60));

        assert_eq!(cfg.get_max_recv_message_size(), Some(8 * 1024 * 1024));
        assert_eq!(cfg.get_max_concurrent_streams(), Some(100));
        assert_eq!(cfg.get_initial_connection_window_size(), Some(1024 * 1024));
        assert_eq!(cfg.get_initial_stream_window_size(), Some(512 * 1024));
        assert_eq!(cfg.get_keep_alive_interval(), Some(Duration::from_secs(30)));
        assert_eq!(cfg.get_keep_alive_timeout(), Some(Duration::from_secs(10)));
        assert_eq!(cfg.get_max_connection_age(), Some(Duration::from_secs(300)));
        assert_eq!(
            cfg.get_max_connection_age_grace(),
            Some(Duration::from_secs(15))
        );
        assert_eq!(cfg.get_handshake_timeout(), Some(Duration::from_secs(60)));
    }

    #[test]
    fn initial_connection_window_size_ignores_values_above_http2_maximum() {
        let cfg = Http2Config::new()
            .with_initial_connection_window_size(MAX_WINDOW_SIZE)
            .with_initial_connection_window_size(MAX_WINDOW_SIZE + 1);

        assert_eq!(
            cfg.get_initial_connection_window_size(),
            Some(MAX_WINDOW_SIZE)
        );
    }

    #[test]
    fn initial_stream_window_size_ignores_values_above_http2_maximum() {
        let cfg = Http2Config::new()
            .with_initial_stream_window_size(MAX_WINDOW_SIZE)
            .with_initial_stream_window_size(MAX_WINDOW_SIZE + 1);

        assert_eq!(cfg.get_initial_stream_window_size(), Some(MAX_WINDOW_SIZE));
    }
}
