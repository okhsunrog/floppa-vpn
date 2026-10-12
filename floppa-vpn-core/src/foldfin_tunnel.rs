//! Foldfin engine adapter. Host networking and reconnect policy stay in the actor.
use crate::{backend::BackendError, foldfin_config::FoldfinVpnConfig};
use foldfin::{
    client::Client,
    packet::PacketDevice,
    session::{Event, Session},
};
use std::{
    net::SocketAddr,
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::net::TcpSocket;

pub(crate) struct FoldfinTunnel {
    session: Session,
    config: FoldfinVpnConfig,
    endpoint: SocketAddr,
    fwmark: Option<u32>,
    started: Instant,
    last_probe: Mutex<Option<Instant>>,
}
fn error(error: impl std::fmt::Display) -> BackendError {
    BackendError::Engine {
        detail: format!("Foldfin: {error}"),
    }
}

impl FoldfinTunnel {
    pub(crate) async fn start(
        config: &FoldfinVpnConfig,
        endpoint: SocketAddr,
        device: PacketDevice,
        fwmark: Option<u32>,
    ) -> Result<Self, BackendError> {
        let client = Self::dial(config, endpoint, fwmark).await?;
        let mut session = Session::new(config.policy(), device).map_err(error)?;
        session.recover(client).await.map_err(error)?;
        Ok(Self {
            session,
            config: config.clone(),
            endpoint,
            fwmark,
            started: Instant::now(),
            last_probe: Mutex::new(None),
        })
    }

    async fn dial(
        config: &FoldfinVpnConfig,
        endpoint: SocketAddr,
        fwmark: Option<u32>,
    ) -> Result<Client, BackendError> {
        if config.transport() == crate::foldfin_config::TransportPolicy::H3Only {
            return Err(error("H3 VPN bridge is not available in this build"));
        }
        let socket = if endpoint.is_ipv4() {
            TcpSocket::new_v4()
        } else {
            TcpSocket::new_v6()
        }
        .map_err(error)?;
        #[cfg(target_os = "linux")]
        if let Some(mark) = fwmark {
            use std::os::fd::AsRawFd;
            // SAFETY: socket and mark are live and the option expects a u32.
            if unsafe {
                libc::setsockopt(
                    socket.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_MARK,
                    (&mark as *const u32).cast(),
                    std::mem::size_of::<u32>() as libc::socklen_t,
                )
            } != 0
            {
                let cause = std::io::Error::last_os_error();
                return Err(if cause.kind() == std::io::ErrorKind::PermissionDenied {
                    BackendError::PermissionDenied {
                        detail: cause.to_string(),
                    }
                } else {
                    error(cause)
                });
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = fwmark;
        #[cfg(target_os = "android")]
        {
            use std::os::fd::AsRawFd;
            crate::tunnel::protect_tcp_socket(socket.as_raw_fd())?;
        }
        let tcp = tokio::time::timeout(foldfin::DEADLINE, socket.connect(endpoint))
            .await
            .map_err(error)?
            .map_err(error)?;
        let client = Client::connect_over(config.client_config().map_err(error)?, tcp)
            .await
            .map_err(error)?;
        Ok(client)
    }

    pub(crate) async fn network_changed(&mut self) -> Result<(), BackendError> {
        self.session.underlay_changed().await;
        *self.last_probe.lock().unwrap() = None;
        let client = Self::dial(&self.config, self.endpoint, self.fwmark).await?;
        self.session.recover(client).await.map_err(error)?;
        self.probe().await
    }

    pub(crate) fn stats(&self) -> foldfin::stats::TrafficStats {
        self.session.stats()
    }
    pub(crate) fn duration(&self) -> Duration {
        self.started.elapsed()
    }
    pub(crate) fn last_rx(&self) -> Option<Duration> {
        let payload = self.stats().since_last_rx_payload;
        let probe = self.last_probe.lock().unwrap().map(|t| t.elapsed());
        match (payload, probe) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
    pub(crate) fn silence(&self) -> Duration {
        self.last_rx().unwrap_or_else(|| self.duration())
    }
    pub(crate) fn alive(&self) -> bool {
        !matches!(self.session.snapshot(), Event::Closed)
    }
    pub(crate) async fn probe(&self) -> Result<(), BackendError> {
        // Capsule confirmation is required; H2 PING alone cannot verify a VPN session.
        let mut state = self.session.subscribe();
        tokio::time::timeout(foldfin::DEADLINE, async {
            loop {
                match state.borrow_and_update().clone() {
                    Event::Running => break Ok(()),
                    Event::Preparing | Event::Recovering => (),
                    other => break Err(error(format!("session ended: {other:?}"))),
                }
                state.changed().await.map_err(error)?;
            }
        })
        .await
        .map_err(error)??;
        self.session.probe_carrier().await.map_err(error)?;
        *self.last_probe.lock().unwrap() = Some(Instant::now());
        Ok(())
    }
    pub(crate) async fn stop(mut self) -> Result<(), BackendError> {
        self.session.stop().await.map_err(error)
    }
}
