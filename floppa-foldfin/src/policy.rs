//! Host IP isolation and plan bandwidth policy, outside the protocol engine.
use foldfin::packet::{PacketDevice, PacketDeviceFactory, PacketRecv, PacketSend};
use ipnet::IpNet;
use std::{
    future::Future,
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::time::Sleep;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Policy {
    pub pools: Vec<IpNet>,
    pub gateways: Vec<IpAddr>,
    pub private: Vec<IpNet>,
    pub speed_mbps: Option<u32>,
}
impl Policy {
    fn allows(&self, ip: IpAddr) -> bool {
        if self.pools.iter().any(|p| p.contains(&ip)) {
            return false;
        }
        if ip.is_unspecified() || ip.is_multicast() || ip.is_loopback() {
            return false;
        }
        match ip {
            IpAddr::V4(v) => {
                let [a, _, _, _] = v.octets();
                if v.is_link_local() || a == 0 || a >= 224 {
                    return false;
                }
            }
            IpAddr::V6(v) => {
                if v.is_unicast_link_local() || v.to_ipv4_mapped().is_some() {
                    return false;
                }
            }
        };
        floppa_core::foldfin::public_destination(ip) || self.private.iter().any(|p| p.contains(&ip))
    }
}
fn addresses(packet: &[u8]) -> Option<(IpAddr, IpAddr)> {
    match packet.first()? >> 4 {
        4 => Some((
            Ipv4Addr::from(<[u8; 4]>::try_from(packet.get(12..16)?).ok()?).into(),
            Ipv4Addr::from(<[u8; 4]>::try_from(packet.get(16..20)?).ok()?).into(),
        )),
        6 => Some((
            Ipv6Addr::from(<[u8; 16]>::try_from(packet.get(8..24)?).ok()?).into(),
            Ipv6Addr::from(<[u8; 16]>::try_from(packet.get(24..40)?).ok()?).into(),
        )),
        _ => None,
    }
}

// Kernel/router ICMP errors can have a private source even for public destinations.
// Preserve PMTU and TTL errors only when the quoted packet originated at this client.
fn related_icmp_error(packet: &[u8], policy: &Policy) -> bool {
    let Some((source, destination)) = addresses(packet) else {
        return false;
    };
    if source.is_unspecified()
        || source.is_multicast()
        || source.is_loopback()
        || (policy.pools.iter().any(|p| p.contains(&source)) && !policy.gateways.contains(&source))
    {
        return false;
    }
    let header = match packet.first().map(|b| b >> 4) {
        Some(4) if packet.get(9) == Some(&1) => {
            let offset = usize::from(packet[0] & 15) * 4;
            if offset < 20 || !matches!(packet.get(offset), Some(3 | 11 | 12)) {
                return false;
            }
            offset + 8
        }
        Some(6) if packet.get(6) == Some(&58) && matches!(packet.get(40), Some(1..=4)) => 48,
        _ => return false,
    };
    packet
        .get(header..)
        .and_then(addresses)
        .is_some_and(|(quoted_source, _)| quoted_source == destination)
}

struct Budget {
    rate: Option<f64>,
    credit: f64,
    updated: Instant,
    sleep: Option<Pin<Box<Sleep>>>,
}
impl Budget {
    fn new(mbps: Option<u32>) -> Self {
        Self {
            rate: mbps.map(|n| f64::from(n) * 125_000.0),
            credit: 65536.0,
            updated: Instant::now(),
            sleep: None,
        }
    }
    fn poll(&mut self, cx: &mut Context<'_>, bytes: usize) -> Poll<()> {
        let Some(rate) = self.rate else {
            return Poll::Ready(());
        };
        if let Some(sleep) = &mut self.sleep {
            std::task::ready!(sleep.as_mut().poll(cx));
            self.sleep = None;
        }
        let now = Instant::now();
        self.credit =
            (self.credit + now.duration_since(self.updated).as_secs_f64() * rate).min(65536.0);
        self.updated = now;
        if self.credit >= bytes as f64 {
            self.credit -= bytes as f64;
            return Poll::Ready(());
        }
        self.sleep = Some(Box::pin(tokio::time::sleep(Duration::from_secs_f64(
            (bytes as f64 - self.credit) / rate,
        ))));
        // Register the waker now; constructing Sleep alone does not register it.
        if self.sleep.as_mut().unwrap().as_mut().poll(cx).is_ready() {
            cx.waker().wake_by_ref();
        }
        Poll::Pending
    }
}

pub(crate) struct Factory {
    pub inner: Arc<dyn PacketDeviceFactory>,
    pub policy: Arc<Policy>,
}
impl PacketDeviceFactory for Factory {
    fn open(&self) -> io::Result<PacketDevice> {
        let device = self.inner.open()?;
        Ok(PacketDevice::new(
            Reader {
                inner: device.recv,
                policy: self.policy.clone(),
                budget: Budget::new(self.policy.speed_mbps),
                pending: Vec::new(),
                bytes: metrics::counter!("floppa_foldfin_tx_bytes_total"),
                drops: metrics::counter!("floppa_foldfin_policy_drops_total"),
            },
            Writer {
                inner: device.send,
                policy: self.policy.clone(),
                budget: Budget::new(self.policy.speed_mbps),
                admitted: false,
                bytes: metrics::counter!("floppa_foldfin_rx_bytes_total"),
                drops: metrics::counter!("floppa_foldfin_policy_drops_total"),
            },
        ))
    }
}
struct Reader {
    inner: Box<dyn PacketRecv>,
    policy: Arc<Policy>,
    budget: Budget,
    pending: Vec<u8>,
    bytes: metrics::Counter,
    drops: metrics::Counter,
}
impl PacketRecv for Reader {
    fn poll_recv(&mut self, cx: &mut Context<'_>, buffer: &mut [u8]) -> Poll<io::Result<usize>> {
        if !self.pending.is_empty() {
            std::task::ready!(self.budget.poll(cx, self.pending.len()));
            let n = self.pending.len();
            if n > buffer.len() {
                return Poll::Ready(Err(io::ErrorKind::InvalidInput.into()));
            }
            buffer[..n].copy_from_slice(&self.pending);
            self.pending.clear();
            self.bytes.increment(n as u64);
            return Poll::Ready(Ok(n));
        }
        for _ in 0..32 {
            let n = std::task::ready!(self.inner.poll_recv(cx, buffer))?;
            if n == 0 {
                return Poll::Ready(Ok(0));
            }
            if !addresses(&buffer[..n]).is_some_and(|(src, _)| self.policy.allows(src))
                && !related_icmp_error(&buffer[..n], &self.policy)
            {
                self.drops.increment(1);
                continue;
            }
            if self.budget.poll(cx, n).is_pending() {
                self.pending.extend_from_slice(&buffer[..n]);
                return Poll::Pending;
            }
            self.bytes.increment(n as u64);
            return Poll::Ready(Ok(n));
        }
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}
struct Writer {
    inner: Box<dyn PacketSend>,
    policy: Arc<Policy>,
    budget: Budget,
    admitted: bool,
    bytes: metrics::Counter,
    drops: metrics::Counter,
}
impl PacketSend for Writer {
    fn poll_send(&mut self, cx: &mut Context<'_>, packet: &[u8]) -> Poll<io::Result<()>> {
        if !addresses(packet).is_some_and(|(_, dst)| self.policy.allows(dst)) {
            self.drops.increment(1);
            return Poll::Ready(Ok(()));
        }
        if !self.admitted {
            std::task::ready!(self.budget.poll(cx, packet.len()));
            self.admitted = true;
        }
        let result = std::task::ready!(self.inner.poll_send(cx, packet));
        self.admitted = false;
        if result.is_ok() {
            self.bytes.increment(packet.len() as u64);
        }
        Poll::Ready(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn internet_grants_never_imply_private_or_client_access() {
        let p = Policy {
            gateways: vec!["10.33.0.1".parse().unwrap(), "fd33::1".parse().unwrap()],
            pools: vec![
                "10.33.0.0/24".parse().unwrap(),
                "fd33::/120".parse().unwrap(),
            ],
            private: vec![
                "10.44.0.0/24".parse().unwrap(),
                "fd44::/120".parse().unwrap(),
            ],
            speed_mbps: None,
        };
        for ip in ["1.1.1.1", "2606:4700:4700::1111", "10.44.0.5", "fd44::5"] {
            assert!(p.allows(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "10.33.0.3",
            "fd33::3",
            "10.55.0.5",
            "fd55::5",
            "127.0.0.1",
            "169.254.169.254",
            "::ffff:1.1.1.1",
        ] {
            assert!(!p.allows(ip.parse().unwrap()), "{ip}");
        }
    }
    #[tokio::test]
    async fn bandwidth_budget_resumes_after_backpressure() {
        let mut budget = Budget::new(Some(1));
        budget.credit = 0.0;
        let start = Instant::now();
        std::future::poll_fn(|cx| budget.poll(cx, 1500)).await;
        assert!(start.elapsed() >= Duration::from_millis(10));
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
