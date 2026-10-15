//! Linux transport sockets must bypass the Floppa routing table from their first packet.
use std::io;
use std::os::fd::{AsRawFd, RawFd};

pub(crate) const FWMARK: u32 = 0x666c6f70;

pub(crate) fn set(fd: RawFd) -> io::Result<()> {
    // SAFETY: the caller owns a live socket; the value pointer and length match u32.
    let result = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_MARK,
            (&FWMARK as *const u32).cast(),
            std::mem::size_of::<u32>() as libc::socklen_t,
        )
    };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    Err(io::Error::new(
        error.kind(),
        format!(
            "cannot set required SO_MARK on VPN socket: {error}. Run through the VPN service or install CAP_NET_ADMIN on the standalone binary; unmarked transport is not supported"
        ),
    ))
}

/// Exercise the actual operation before changing links, routes or DNS. Do not infer permissions
/// from /proc: the kernel also permits CAP_NET_RAW on recent Linux and may enforce socket policy.
pub(crate) fn preflight() -> io::Result<()> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0")?;
    set(socket.as_raw_fd())?;
    match std::net::UdpSocket::bind("[::]:0") {
        Ok(socket) => set(socket.as_raw_fd()),
        Err(e)
            if matches!(
                e.raw_os_error(),
                Some(libc::EAFNOSUPPORT | libc::EADDRNOTAVAIL)
            ) =>
        {
            Ok(())
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marking_is_verified_or_fails_explicitly() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        match set(socket.as_raw_fd()) {
            Ok(()) => {
                let mut mark = 0u32;
                let mut len = std::mem::size_of_val(&mark) as libc::socklen_t;
                let result = unsafe {
                    libc::getsockopt(
                        socket.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_MARK,
                        (&mut mark as *mut u32).cast(),
                        &mut len,
                    )
                };
                assert_eq!(result, 0);
                assert_eq!(mark, FWMARK);
            }
            Err(e) => {
                assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
                assert!(e.to_string().contains("CAP_NET_ADMIN"));
                assert!(
                    e.to_string()
                        .contains("unmarked transport is not supported")
                );
            }
        }
    }
}
