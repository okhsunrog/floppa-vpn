//! Foldfin engine adapter. Host networking and reconnect policy stay in the actor.
use crate::{backend::BackendError, foldfin_config::FoldfinVpnConfig};
use foldfin::{
    client::Client,
    packet::PacketDevice,
    session::{Event, Session},
};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::net::TcpSocket;

pub(crate) struct FoldfinTunnel {
    session: Session,
    config: FoldfinVpnConfig,
    endpoint: SocketAddr,
    underlay: Arc<dyn crate::underlay::Underlay>,
    started: Instant,
    last_probe: Mutex<Option<Instant>>,
}
fn error(error: impl std::fmt::Display) -> BackendError {
    BackendError::Engine {
        detail: format!("Foldfin: {error}"),
    }
}

fn connection_error(cause: foldfin::error::Error) -> BackendError {
    let detail = format!("Foldfin: {cause}");
    if cause.permits_transport_fallback() {
        BackendError::EndpointUnavailable { detail }
    } else {
        BackendError::ConnectionRejected { detail }
    }
}
fn timeout_error(_: tokio::time::error::Elapsed) -> BackendError {
    connection_error(foldfin::error::Error::Timeout("connection attempt"))
}

fn socket_error(cause: std::io::Error) -> BackendError {
    if cause.kind() == std::io::ErrorKind::PermissionDenied {
        BackendError::PermissionDenied {
            detail: cause.to_string(),
        }
    } else {
        error(cause)
    }
}

impl FoldfinTunnel {
    pub(crate) async fn start(
        config: &FoldfinVpnConfig,
        endpoint: SocketAddr,
        device: PacketDevice,
        underlay: Arc<dyn crate::underlay::Underlay>,
    ) -> Result<Self, BackendError> {
        let mut session = Session::new(config.policy(), device).map_err(error)?;
        Self::attach(&mut session, config, endpoint, underlay.clone()).await?;
        Ok(Self {
            session,
            config: config.clone(),
            endpoint,
            underlay,
            started: Instant::now(),
            last_probe: Mutex::new(None),
        })
    }

    async fn attach(
        session: &mut Session,
        config: &FoldfinVpnConfig,
        endpoint: SocketAddr,
        underlay: Arc<dyn crate::underlay::Underlay>,
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
                underlay.prepare_udp(&socket).map_err(socket_error)?;
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
                    Err(cause) => return Err(connection_error(cause)),
                }
            }
            let client = Self::dial(config, endpoint, underlay.clone()).await?;
            session.recover(client).await.map_err(connection_error)
        })
        .await
        .map_err(timeout_error)?
    }

    async fn dial(
        config: &FoldfinVpnConfig,
        endpoint: SocketAddr,
        underlay: Arc<dyn crate::underlay::Underlay>,
    ) -> Result<Client, BackendError> {
        let socket = if endpoint.is_ipv4() {
            TcpSocket::new_v4()
        } else {
            TcpSocket::new_v6()
        }
        .map_err(error)?;
        underlay.prepare_tcp(&socket).map_err(socket_error)?;
        let tcp = tokio::time::timeout(foldfin::DEADLINE, socket.connect(endpoint))
            .await
            .map_err(timeout_error)?
            .map_err(|e| connection_error(e.into()))?;
        let client = Client::connect_over(config.client_config().map_err(connection_error)?, tcp)
            .await
            .map_err(connection_error)?;
        Ok(client)
    }

    pub(crate) fn service_generation(&self) -> Option<u64> {
        self.underlay.service_generation()
    }

    pub(crate) async fn network_changed(&mut self) -> Result<(), BackendError> {
        self.session.underlay_changed().await;
        *self.last_probe.lock().unwrap() = None;
        Self::attach(
            &mut self.session,
            &self.config,
            self.endpoint,
            self.underlay.clone(),
        )
        .await?;
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
        .map_err(timeout_error)??;
        self.session
            .probe_carrier()
            .await
            .map_err(connection_error)?;
        *self.last_probe.lock().unwrap() = Some(Instant::now());
        Ok(())
    }
    pub(crate) async fn stop(mut self) -> Result<(), BackendError> {
        self.session.stop().await.map_err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_authorization_and_protocol_failures_are_terminal() {
        use foldfin::error::Error;
        for cause in [
            Error::Rejected(401),
            Error::Rejected(403),
            Error::Rejected(409),
            Error::InvalidConfig("invalid identity".into()),
            Error::Unsupported("invalid capsule negotiation"),
        ] {
            let failure = crate::actor::types::AttemptError::Backend {
                error: connection_error(cause),
            };
            assert!(failure.is_fatal_for_cycle());
            assert!(!failure.permits_endpoint_retry());
        }
        for cause in [
            Error::Timeout("handshake"),
            Error::Io(std::io::ErrorKind::ConnectionRefused.into()),
            Error::PathMtu {
                required: 1280,
                available: 1200,
            },
        ] {
            let failure = crate::actor::types::AttemptError::Backend {
                error: connection_error(cause),
            };
            assert!(!failure.is_fatal_for_cycle());
            assert!(failure.permits_endpoint_retry());
        }
    }
}
