//! Desktop underlay observations. No HTTP/DNS probe: a route is evidence of a path,
//! not of Internet reachability. Failed observations are Unknown, never Offline.
//! Sampling also catches lost/coalesced OS notifications and follows USB tethering.
use crate::actor::handle::Command;
use crate::actor::types::Link;
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Paths(Vec<String>);

async fn output(program: &str, args: &[&str]) -> Result<Vec<u8>, ()> {
    let mut command = tokio::process::Command::new(program);
    command.args(args).kill_on_drop(true);
    #[cfg(target_os = "windows")]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    let output = tokio::time::timeout(Duration::from_secs(3), command.output())
        .await
        .map_err(|_| ())?
        .map_err(|_| ())?;
    if !output.status.success() {
        return Err(());
    }
    Ok(output.stdout)
}

#[cfg(target_os = "linux")]
async fn paths(iface: &str) -> Result<Paths, ()> {
    let links = output("ip", &["-j", "-d", "address", "show"]).await?;
    let v4 = output(
        "ip",
        &["-j", "-4", "route", "show", "table", "main", "default"],
    )
    .await?;
    let v6 = output(
        "ip",
        &["-j", "-6", "route", "show", "table", "main", "default"],
    )
    .await?;
    linux_paths(&links, &v4, &v6, iface)
}

#[cfg(target_os = "linux")]
fn linux_paths(links: &[u8], v4: &[u8], v6: &[u8], iface: &str) -> Result<Paths, ()> {
    use serde_json::Value;
    let links: Vec<Value> = serde_json::from_slice(links).map_err(|_| ())?;
    let mut paths = Vec::new();
    for (family, raw) in [(4, v4), (6, v6)] {
        let routes: Vec<Value> = serde_json::from_slice(raw).map_err(|_| ())?;
        let mut available = false;
        for route in routes {
            let Some(dev) = route["dev"].as_str() else {
                continue;
            };
            if dev == iface || dev == "lo" || route["type"].as_str().is_some_and(|t| t != "unicast")
            {
                continue;
            }
            let Some(link) = links.iter().find(|l| l["ifname"] == dev) else {
                continue;
            };
            let flags = link["flags"].as_array().ok_or(())?;
            if !flags.iter().any(|f| f == "UP")
                || link["operstate"] == "DOWN"
                || link["operstate"] == "LOWERLAYERDOWN"
            {
                continue;
            }
            if route["flags"]
                .as_array()
                .is_some_and(|f| f.iter().any(|f| f == "linkdown"))
            {
                continue;
            }
            // A tun/WireGuard corporate route is not proof of an underlying path.
            if matches!(
                link["linkinfo"]["info_kind"].as_str(),
                Some(
                    "tun"
                        | "ovpn"
                        | "wireguard"
                        | "amneziawg"
                        | "ipip"
                        | "sit"
                        | "gre"
                        | "ip6tnl"
                        | "vti"
                        | "vti6"
                )
            ) {
                continue;
            }
            available = true;
        }
        if available {
            paths.push(family.to_string());
        }
    }
    paths.sort();
    paths.dedup();
    Ok(Paths(paths))
}

#[cfg(target_os = "windows")]
async fn paths(_: &str) -> Result<Paths, ()> {
    // Physical adapters include WLAN/Ethernet/USB. Exclude virtual VPN adapters, whose
    // surviving routes must not claim connectivity in airplane mode.
    let raw = output("powershell.exe", &["-NoProfile", "-NonInteractive", "-Command",
        "$ErrorActionPreference='Stop'; $ids=@(Get-NetAdapter -Physical | Where-Object Status -eq 'Up' | Select-Object -ExpandProperty ifIndex); @(Get-NetRoute | Where-Object { ($_.DestinationPrefix -eq '0.0.0.0/0' -or $_.DestinationPrefix -eq '::/0') -and $ids.Contains($_.InterfaceIndex) } | ForEach-Object { '{0}:{1}:{2}:{3}' -f $_.AddressFamily,$_.InterfaceIndex,$_.NextHop,$_.RouteMetric } | Sort-Object -Unique) | ConvertTo-Json -Compress"]).await?;
    if raw.iter().all(u8::is_ascii_whitespace) {
        return Ok(Paths(vec![]));
    }
    let value: serde_json::Value = serde_json::from_slice(&raw).map_err(|_| ())?;
    let paths = match value {
        serde_json::Value::String(s) => vec![s],
        serde_json::Value::Array(a) => a
            .into_iter()
            .map(|v| v.as_str().map(str::to_owned).ok_or(()))
            .collect::<Result<_, _>>()?,
        serde_json::Value::Null => vec![],
        _ => return Err(()),
    };
    Ok(Paths(paths))
}

pub(crate) async fn initial(iface: &str) -> Link {
    match paths(iface).await {
        Ok(Paths(paths)) if paths.is_empty() => Link::Offline,
        Ok(_) => Link::Online,
        Err(()) => Link::Unknown,
    }
}

#[cfg(target_os = "linux")]
struct RouteEvents(tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>);
#[cfg(target_os = "linux")]
impl RouteEvents {
    fn subscribe() -> std::io::Result<Self> {
        use std::os::fd::{AsRawFd, FromRawFd};
        // Read-only rtnetlink subscription; parsing remains in the snapshot reader.
        let raw = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                libc::NETLINK_ROUTE,
            )
        };
        if raw < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
        let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        addr.nl_family = libc::AF_NETLINK as u16;
        addr.nl_groups = (libc::RTMGRP_LINK
            | libc::RTMGRP_IPV4_IFADDR
            | libc::RTMGRP_IPV6_IFADDR
            | libc::RTMGRP_IPV4_ROUTE
            | libc::RTMGRP_IPV6_ROUTE) as u32;
        let result = unsafe {
            libc::bind(
                fd.as_raw_fd(),
                (&addr as *const libc::sockaddr_nl).cast(),
                std::mem::size_of_val(&addr) as u32,
            )
        };
        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self(tokio::io::unix::AsyncFd::new(fd)?))
    }
    async fn changed(&self) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        loop {
            let mut ready = self.0.readable().await?;
            match ready.try_io(|fd| {
                let mut buffer = [0u8; 8192];
                let size = unsafe {
                    libc::recv(fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len(), 0)
                };
                if size < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            }) {
                Ok(result) => return result,
                Err(_) => continue,
            }
        }
    }
}

pub(crate) async fn watch(iface: String, tx: mpsc::Sender<Command>) {
    #[cfg(target_os = "linux")]
    let mut events = RouteEvents::subscribe().ok();
    let mut previous: Option<Paths> = None;
    let mut previous_link = Link::Unknown;
    loop {
        if tx.is_closed() {
            return;
        }
        let observation = paths(&iface).await;
        let link = match &observation {
            Ok(Paths(paths)) if paths.is_empty() => Link::Offline,
            Ok(_) => Link::Online,
            Err(()) => Link::Unknown,
        };
        if link != previous_link || previous.is_none() {
            if tx.send(Command::LinkChanged(link)).await.is_err() {
                return;
            }
            previous_link = link;
        }
        if let Ok(current) = observation {
            #[cfg(target_os = "windows")]
            if !current.0.is_empty()
                && previous.as_ref().is_some_and(|old| old != &current)
                && tx.send(Command::NetworkChanged).await.is_err()
            {
                return;
            }
            previous = Some(current);
        }
        #[cfg(target_os = "linux")]
        if let Some(source) = &events {
            tokio::select! {
                _ = tx.closed() => return,
                result = source.changed() => {
                    floppa_net::network_changed();
                    if result.is_err() { events = None; }
                },
                _ = tokio::time::sleep(Duration::from_secs(30)) => {},
            }
        } else {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        #[cfg(target_os = "windows")]
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    fn availability_does_not_depend_on_metrics_or_address_changes() {
        let links = br#"[{"ifindex":2,"ifname":"eth0","flags":["UP"],"addr_info":[{"scope":"global","local":"192.0.2.2"}]},{"ifindex":3,"ifname":"wlan0","flags":["UP"]}]"#;
        let routes = br#"[{"dev":"wlan0","metric":600},{"dev":"eth0","metric":100}]"#;
        let before = linux_paths(links, routes, b"[]", "floppa0").unwrap();
        assert_eq!(before.0, vec!["4"]);
        let changed = String::from_utf8(links.to_vec())
            .unwrap()
            .replace("192.0.2.2", "192.0.2.3");
        assert_eq!(
            before,
            linux_paths(changed.as_bytes(), routes, b"[]", "floppa0").unwrap()
        );
        let wifi = br#"[{"dev":"wlan0","metric":50},{"dev":"eth0","metric":100}]"#;
        assert_eq!(before, linux_paths(links, wifi, b"[]", "floppa0").unwrap());
    }
    #[test]
    fn ignores_own_vpn_and_disconnected_links_but_accepts_usb_and_ipv6() {
        let links = br#"[{"ifindex":1,"ifname":"floppa0","flags":["UP"],"operstate":"UNKNOWN"},{"ifindex":2,"ifname":"usb0","flags":["UP"],"operstate":"UP"},{"ifindex":3,"ifname":"wlan0","flags":["UP"],"operstate":"DOWN"},{"ifindex":4,"ifname":"tun0","flags":["UP"],"linkinfo":{"info_kind":"tun"}}]"#;
        let routes = br#"[{"dev":"floppa0"},{"dev":"wlan0"},{"dev":"tun0"}]"#;
        assert!(
            linux_paths(links, routes, b"[]", "floppa0")
                .unwrap()
                .0
                .is_empty()
        );
        assert_eq!(
            linux_paths(
                links,
                routes,
                br#"[{"dev":"usb0","gateway":"fe80::1"}]"#,
                "floppa0"
            )
            .unwrap()
            .0
            .len(),
            1
        );
        assert!(linux_paths(b"bad", b"[]", b"[]", "floppa0").is_err());
    }
}
