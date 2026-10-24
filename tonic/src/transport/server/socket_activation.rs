// Shared helpers for adopting sockets passed in by a socket-activation manager

use std::os::unix::io::{BorrowedFd, RawFd};

const SD_LISTEN_FDS_START: RawFd = 3;

// Reads the socket-activation env vars (LISTEN_PID/LISTEN_FDS) and returns the
// first inherited fd accepted by `matches`, or None if none were passed to us.
pub(super) fn find_preallocated_fd<F>(matches: F) -> Option<RawFd>
where
    F: Fn(RawFd) -> bool,
{
    let listen_pid: u32 = std::env::var("LISTEN_PID").ok()?.parse().ok()?;
    let n_fds: i32 = std::env::var("LISTEN_FDS").ok()?.parse().ok()?;

    scan_preallocated_fds(listen_pid, n_fds, matches)
}

pub(super) fn scan_preallocated_fds<F>(listen_pid: u32, n_fds: i32, matches: F) -> Option<RawFd>
where
    F: Fn(RawFd) -> bool,
{
    if listen_pid != std::process::id() {
        return None;
    }

    let end = SD_LISTEN_FDS_START.checked_add(n_fds)?;

    (SD_LISTEN_FDS_START..end).find(|&fd| is_listening_stream_socket(fd) && matches(fd))
}

/// Returns `true` if `fd` refers to a stream socket in the listening state.
pub(super) fn is_listening_stream_socket(fd: RawFd) -> bool {
    // SAFETY: `fd` is a valid, open descriptor from the activation range. It is
    // only borrowed, not owned: the `BorrowedFd` does not close it on drop and
    // does not outlive this function.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    let sock = socket2::SockRef::from(&borrowed);

    matches!(sock.r#type(), Ok(socket2::Type::STREAM)) && matches!(sock.is_listener(), Ok(true))
}
