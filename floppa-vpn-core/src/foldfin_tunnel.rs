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
        let mut session = Session::new(config.policy(), device).map_err(error)?;
        Self::attach(&mut session, config, endpoint, fwmark).await?;
        Ok(Self {
            session,
            config: config.clone(),
            endpoint,
            fwmark,
            started: Instant::now(),
            last_probe: Mutex::new(None),
        })
    }

    async fn attach(
        session: &mut Session,
        config: &FoldfinVpnConfig,
        endpoint: SocketAddr,
        fwmark: Option<u32>,
    ) -> Result<(), BackendError> {
        use crate::foldfin_config::TransportPolicy;
        tokio::time::timeout(foldfin::DEADLINE, async {
            if config.transport() != TransportPolicy::H2Only {
                // Prepare/protect the UDP socket before Quinn can transmit anything.
                let bind = if endpoint.is_ipv4() {
                    "0.0.0.0:0"
                } else {
                    "[::]:0"
                };
                let socket = std::net::UdpSocket::bind(bind).map_err(error)?;
                socket.set_nonblocking(true).map_err(error)?;
                #[cfg(unix)]
                {
                    use std::os::fd::AsRawFd;
                    Self::prepare_fd(socket.as_raw_fd(), fwmark)?;
                }
                let attempt = async {
                    let client = foldfin::h3_client::Client::connect_over(
                        config.client_config()?,
                        socket,
                        endpoint,
                    )
                    .await?;
                    session.recover(client).await
                };
                let budget = if config.transport() == TransportPolicy::Auto {
                    Duration::from_secs(2)
                } else {
                    foldfin::DEADLINE
                };
                let result = tokio::time::timeout(budget, attempt)
                    .await
                    .unwrap_or(Err(foldfin::error::Error::Timeout("H3 candidate")));
                match result {
                    Ok(()) => return Ok(()),
                    Err(cause)
                        if config.transport() == TransportPolicy::Auto
                            && cause.permits_transport_fallback() =>
                    {
                        session.underlay_changed().await;
                    }
                    Err(cause) => return Err(error(cause)),
                }
            }
            let client = Self::dial(config, endpoint, fwmark).await?;
            session.recover(client).await.map_err(error)
        })
        .await
        .map_err(error)?
    }

    #[cfg(unix)]
    fn prepare_fd(fd: std::os::fd::RawFd, fwmark: Option<u32>) -> Result<(), BackendError> {
        #[cfg(target_os = "linux")]
        if let Some(mark) = fwmark {
            // SAFETY: the caller retains a live socket and SO_MARK expects a u32.
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
                return Err(error(std::io::Error::last_os_error()));
            }
        }
        #[cfg(target_os = "android")]
        crate::tunnel::protect_tcp_socket(fd)?;
        let _ = (fd, fwmark);
        Ok(())
    }

    async fn dial(
        config: &FoldfinVpnConfig,
        endpoint: SocketAddr,
        fwmark: Option<u32>,
    ) -> Result<Client, BackendError> {
        let socket = if endpoint.is_ipv4() {
            TcpSocket::new_v4()
        } else {
            TcpSocket::new_v6()
        }
        .map_err(error)?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            Self::prepare_fd(socket.as_raw_fd(), fwmark)?;
        }
        #[cfg(not(unix))]
        let _ = fwmark;
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
        Self::attach(&mut self.session, &self.config, self.endpoint, self.fwmark).await?;
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
