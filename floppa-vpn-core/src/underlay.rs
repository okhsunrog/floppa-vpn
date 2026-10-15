//! Host-owned socket preparation, retained by one Foldfin session.
//! Implementations must fail closed; neither callback is allowed to transmit.
use std::{io, net::UdpSocket};
use tokio::net::TcpSocket;

pub trait Underlay: Send + Sync {
    fn service_generation(&self) -> Option<u64> {
        None
    }
    fn prepare_tcp(&self, socket: &TcpSocket) -> io::Result<()>;
    fn prepare_udp(&self, socket: &UdpSocket) -> io::Result<()>;
}

/// Desktop socket policy. Android must supply its service-owned implementation.
pub struct NativeUnderlay {
    mark: Option<u32>,
}
impl NativeUnderlay {
    pub fn new(mark: Option<u32>) -> Self {
        Self { mark }
    }
    #[cfg(unix)]
    fn prepare(&self, fd: std::os::fd::RawFd) -> io::Result<()> {
        #[cfg(target_os = "android")]
        {
            let _ = (fd, self.mark);
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Android requires a service-owned underlay",
            ))
        }
        #[cfg(target_os = "linux")]
        if let Some(mark) = self.mark {
            // SAFETY: the caller holds the live socket; SO_MARK takes a u32.
            if unsafe {
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_MARK,
                    (&mark as *const u32).cast(),
                    std::mem::size_of::<u32>() as libc::socklen_t,
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = (fd, self.mark);
            Ok(())
        }
    }
}
impl Underlay for NativeUnderlay {
    fn prepare_tcp(&self, socket: &TcpSocket) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            self.prepare(socket.as_raw_fd())
        }
        #[cfg(not(unix))]
        {
            let _ = (socket, self.mark);
            Ok(())
        }
    }
    fn prepare_udp(&self, socket: &UdpSocket) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            self.prepare(socket.as_raw_fd())
        }
        #[cfg(not(unix))]
        {
            let _ = (socket, self.mark);
            Ok(())
        }
    }
}

/// A protector belonging to one service generation, never a process-wide hook.
#[cfg(unix)]
pub struct ServiceUnderlay {
    service: std::sync::Weak<crate::service_state::ServiceState>,
    protect: Box<dyn Fn(std::os::fd::RawFd) -> io::Result<()> + Send + Sync>,
}
#[cfg(unix)]
impl ServiceUnderlay {
    pub fn new(
        service: &std::sync::Arc<crate::service_state::ServiceState>,
        protect: impl Fn(std::os::fd::RawFd) -> io::Result<()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            service: std::sync::Arc::downgrade(service),
            protect: Box::new(protect),
        }
    }
    fn prepare(&self, fd: std::os::fd::RawFd) -> io::Result<()> {
        let service = self.service.upgrade().ok_or_else(|| {
            io::Error::new(io::ErrorKind::PermissionDenied, "VPN service expired")
        })?;
        let active = || {
            if service.phase() == crate::service_state::GenerationPhase::Stopped {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "VPN service generation stopped",
                ))
            } else {
                Ok(())
            }
        };
        active()?;
        (self.protect)(fd)?;
        active()
    }
}
#[cfg(unix)]
impl Underlay for ServiceUnderlay {
    fn service_generation(&self) -> Option<u64> {
        self.service.upgrade().map(|s| s.generation)
    }
    fn prepare_tcp(&self, socket: &TcpSocket) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        self.prepare(socket.as_raw_fd())
    }
    fn prepare_udp(&self, socket: &UdpSocket) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        self.prepare(socket.as_raw_fd())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    #[tokio::test]
    async fn superseded_service_cannot_prepare_sockets_on_its_successor() {
        let registry = crate::service_state::ServiceRegistry::default();
        let first = registry.begin(1);
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let policy = Arc::new(ServiceUnderlay::new(&first, move |_| {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }));
        first.set_underlay(policy.clone()).unwrap();
        assert!(first.set_underlay(policy.clone()).is_err());
        let tcp = TcpSocket::new_v4().unwrap();
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        policy.prepare_tcp(&tcp).unwrap();
        policy.prepare_udp(&udp).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let successor = registry.begin(2);
        assert_eq!(
            policy.prepare_tcp(&tcp).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            policy.prepare_udp(&udp).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_ne!(
            successor.phase(),
            crate::service_state::GenerationPhase::Stopped
        );
        drop(first);
        assert!(policy.prepare_tcp(&tcp).is_err());
    }
    #[tokio::test]
    async fn retirement_during_preparation_fails_closed() {
        let service = Arc::new(crate::service_state::ServiceState::new(3));
        let retired = service.clone();
        let policy = ServiceUnderlay::new(&service, move |_| {
            retired.mark_stopped();
            Ok(())
        });
        assert!(policy.prepare_tcp(&TcpSocket::new_v4().unwrap()).is_err());
    }
}
